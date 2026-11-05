mod alarms;
mod analytics;
mod batcher;
mod config;
mod counter;
mod detector;
mod discover;
mod pipeline;
mod settings;
mod tracker;
mod zones;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use parking_lot::{Mutex, RwLock};
use pipeline::{get as getf, Camera};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::Arc;

#[derive(Default, Clone, Copy)]
struct Gpu {
    util: f64,
    mem_used: f64,
    mem_total: f64,
    power: f64,
}

struct App {
    batcher: RwLock<Option<Arc<batcher::Batcher>>>,
    error: Mutex<Option<String>>,
    hub: Arc<analytics::Hub>,
    cams: Mutex<BTreeMap<u32, Arc<Camera>>>,
    next_id: AtomicU32,
    gpu: Mutex<Gpu>,
    cpu_cores: Mutex<f64>,
    max_batch: usize,
    /// developer mode: the UI also shows frame drops, latency, GPU and batching statistics
    dev: bool,
    /// stream URLs found by "Discover" (token -> url incl. credentials); credentials never go to the browser
    found: Mutex<std::collections::HashMap<String, String>>,
}
type Shared = Arc<App>;

#[derive(Deserialize)]
struct AddReq {
    /// demo preset (1 or 2) ...
    #[serde(default)]
    setup: u32,
    #[serde(default = "default_fps")]
    fps: u32,
    /// ... or a stream URL typed by the user ...
    #[serde(default)]
    url: String,
    /// ... or a stream found by discovery
    #[serde(default)]
    token: String,
    #[serde(default)]
    name: String,
}

fn default_fps() -> u32 {
    30
}

fn slug(s: &str) -> String {
    let v: String = s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    v.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

/// Served from disk when present (edit + refresh while developing), embedded copy otherwise.
async fn index() -> impl IntoResponse {
    let body = tokio::fs::read_to_string("static/index.html").await.unwrap_or_else(|_| include_str!("../static/index.html").to_string());
    ([("content-type", "text/html; charset=utf-8"), ("cache-control", "no-store")], body)
}

const ASSETS: [(&str, &str); 7] = [
    ("model.js", include_str!("../static/model.js")),
    ("alarms.js", include_str!("../static/alarms.js")),
    ("addcam.js", include_str!("../static/addcam.js")),
    ("app.js", include_str!("../static/app.js")),
    ("editor.js", include_str!("../static/editor.js")),
    ("analytics.js", include_str!("../static/analytics.js")),
    ("app.css", include_str!("../static/app.css")),
];

/// /s/<file>: from disk when present (edit + refresh), embedded copy otherwise. Names are whitelisted.
async fn asset(Path(name): Path<String>) -> impl IntoResponse {
    let Some((_, embedded)) = ASSETS.iter().find(|(n, _)| *n == name) else {
        return (StatusCode::NOT_FOUND, [("content-type", "text/plain"), ("cache-control", "no-store")], String::new());
    };
    let body = tokio::fs::read_to_string(format!("static/{name}")).await.unwrap_or_else(|_| embedded.to_string());
    let ct = if name.ends_with(".css") { "text/css" } else { "text/javascript" };
    (StatusCode::OK, [("content-type", ct), ("cache-control", "no-store")], body)
}

const FONTS: [(&str, &[u8]); 4] = [
    ("nunito-200.woff2", include_bytes!("../static/fonts/nunito-200.woff2")),
    ("nunito-300.woff2", include_bytes!("../static/fonts/nunito-300.woff2")),
    ("nunito-400.woff2", include_bytes!("../static/fonts/nunito-400.woff2")),
    ("nunito-600.woff2", include_bytes!("../static/fonts/nunito-600.woff2")),
];
async fn font(Path(name): Path<String>) -> impl IntoResponse {
    match FONTS.iter().find(|(n, _)| *n == name) {
        Some((_, b)) => (StatusCode::OK, [("content-type", "font/woff2"), ("cache-control", "public, max-age=604800")], b.to_vec()),
        None => (StatusCode::NOT_FOUND, [("content-type", "text/plain"), ("cache-control", "no-store")], Vec::new()),
    }
}

async fn get_config(State(app): State<Shared>, Path(id): Path<u32>) -> impl IntoResponse {
    let Some(cam) = app.cams.lock().get(&id).cloned() else { return (StatusCode::NOT_FOUND, Json(json!({}))) };
    let cfg = cam.config.read().clone();
    (StatusCode::OK, Json(json!({"setup": cam.setup, "key": cam.key, "name": cam.name, "classes": config::CLASSES.iter().map(|c| c.0).collect::<Vec<_>>(), "config": cfg})))
}

async fn put_config(State(app): State<Shared>, Path(id): Path<u32>, Json(mut cfg): Json<config::CameraConfig>) -> impl IntoResponse {
    let Some(cam) = app.cams.lock().get(&id).cloned() else { return (StatusCode::NOT_FOUND, Json(json!({}))) };
    cfg.sanitize();
    config::save(&cam.key, &cfg);
    // every running camera that shares this configuration gets it
    for c in app.cams.lock().values().filter(|c| c.key == cam.key) {
        *c.config.write() = cfg.clone();
        c.cfg_ver.fetch_add(1, Relaxed);
    }
    (StatusCode::OK, Json(json!({"config": cfg})))
}

#[derive(Deserialize)]
struct EventsQ {
    #[serde(default)]
    since: u64,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    40_000
}
async fn events(State(app): State<Shared>, axum::extract::Query(q): axum::extract::Query<EventsQ>) -> Json<serde_json::Value> {
    let (ev, occ) = app.hub.query(q.since, q.limit);
    Json(json!({"now": analytics::now_ms(), "events": ev, "occ": occ}))
}

async fn alerts_ws(State(app): State<Shared>, up: WebSocketUpgrade) -> impl IntoResponse {
    up.on_upgrade(move |mut sock| async move {
        let mut rx = app.hub.alerts.subscribe();
        while let Ok(m) = rx.recv().await {
            if sock.send(Message::Text(m)).await.is_err() {
                break;
            }
        }
    })
}

const URL_SCHEMES: [&str; 9] = ["rtsp://", "rtsps://", "rtmp://", "rtmps://", "http://", "https://", "srt://", "udp://", "rtp://"];

/// Starts one camera and remembers it (so it comes back after a restart).
fn spawn_camera(app: &Shared, batcher: Arc<batcher::Batcher>, src: &analytics::Source, persist: bool) -> u32 {
    let id = app.next_id.fetch_add(1, Relaxed) + 1;
    // names identify cameras everywhere (alarms, analytics): make them unique
    let mut name = src.name.clone();
    {
        let cams = app.cams.lock();
        let mut n = 2;
        while cams.values().any(|c| c.name == name) {
            name = format!("{} {n}", src.name);
            n += 1;
        }
    }
    let kind = if src.kind == "demo" {
        pipeline::Src::Demo { setup: src.setup, start_s: pipeline::setups().iter().find(|s| s.id == src.setup).map_or(0, |s| s.start_s), port: 9100 + id as u16 }
    } else {
        pipeline::Src::Url(src.url.clone())
    };
    let cam = pipeline::start(id, name.clone(), src.key.clone(), kind, src.fps, batcher, app.hub.clone());
    app.cams.lock().insert(id, cam);
    if persist {
        app.hub.save_source(&analytics::Source { name, ..src.clone() });
    }
    id
}

async fn add_camera(State(app): State<Shared>, Json(req): Json<AddReq>) -> impl IntoResponse {
    let Some(batcher) = app.batcher.read().clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "engine is still starting"})));
    };
    let src = if !req.url.is_empty() || !req.token.is_empty() {
        let url = if req.token.is_empty() { req.url.trim().to_string() } else { app.found.lock().get(&req.token).cloned().unwrap_or_default() };
        if !URL_SCHEMES.iter().any(|sc| url.starts_with(sc)) {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "Use a stream URL such as rtsp://user:password@192.168.1.20:554/stream1"})));
        }
        let host = url.split("://").nth(1).and_then(|r| r.rsplit('@').next()).and_then(|r| r.split(['/', ':']).next()).unwrap_or("camera").to_string();
        let name = if req.name.trim().is_empty() { format!("Camera {host}") } else { req.name.trim().chars().take(32).collect() };
        analytics::Source { key: format!("cam-{}", slug(&name)), name, kind: "url".into(), setup: 0, fps: 25, url }
    } else {
        let Some(setup) = pipeline::setups().into_iter().find(|s| s.id == req.setup) else {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "unknown setup"})));
        };
        analytics::Source { name: setup.name.to_string(), key: format!("setup{}", setup.id), kind: "demo".into(), setup: setup.id, fps: req.fps.clamp(1, 240), url: String::new() }
    };
    let id = spawn_camera(&app, batcher, &src, true);
    (StatusCode::OK, Json(json!({"id": id})))
}

async fn remove_camera(State(app): State<Shared>, Path(id): Path<u32>) -> StatusCode {
    let cam = app.cams.lock().remove(&id);
    match cam {
        Some(c) => {
            app.hub.delete_source(&c.name);
            tokio::task::spawn_blocking(move || c.stop());
            StatusCode::OK
        }
        None => StatusCode::NOT_FOUND,
    }
}

// ---------------------------------------------------------------- detection settings
/// Models in ./models that follow the `<name>_<H>x<W>.onnx` convention. The resolution is a property of the file.
fn list_models() -> Vec<serde_json::Value> {
    let mut v: Vec<(String, usize, usize, u64)> = std::fs::read_dir("models")
        .map(|d| d.flatten().filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.ends_with(".onnx") || name.contains("traffic-only") {
                return None;
            }
            let (h, w) = settings::parse_res(&name)?;
            Some((name, h, w, e.metadata().map(|m| m.len()).unwrap_or(0)))
        }).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| (a.0.split('_').next(), a.1 * a.2).cmp(&(b.0.split('_').next(), b.1 * b.2)));
    v.into_iter().map(|(name, h, w, size)| {
        let family = name.trim_end_matches(".onnx").split('_').next().unwrap_or("").to_string();
        json!({"file": format!("models/{name}"), "family": family, "w": w, "h": h, "mb": (size as f64 / 1e6).round(),
               "cost": ((w * h) as f64 / (960.0 * 544.0) * 10.0).round() / 10.0, "all_classes": family.starts_with("yolo26")})
    }).collect()
}

fn model_state(app: &App) -> serde_json::Value {
    let st = settings::current();
    let from_env = settings::model_from_env();
    let active = settings::loaded_model();
    let trt = std::env::var("EP").as_deref() == Ok("trt");
    let wanted = settings::effective_batch(&settings::active_model(), trt);
    let d = detector::dims();
    json!({
        "settings": st, "defaults": settings::Settings::default(),
        "models": list_models(),
        "classes": config::CLASSES.iter().map(|c| c.0).collect::<Vec<_>>(),
        "active": {"model": active, "w": d.w, "h": d.h, "max_batch": app.max_batch, "ep": std::env::var("EP").unwrap_or_else(|_| "cuda".into()), "from_env": from_env.is_some(),
                   "auto_batch": settings::auto_batch(settings::parse_res(&st.model).map_or(960, |r| r.1), trt)},
        "restart_needed": wanted != app.max_batch || (from_env.is_none() && st.model != active),
    })
}

async fn model_get(State(app): State<Shared>) -> Json<serde_json::Value> {
    Json(model_state(&app))
}

/// Partial update: only the fields that are sent change. Confidence and overlap apply immediately.
async fn model_put(State(app): State<Shared>, Json(patch): Json<serde_json::Value>) -> impl IntoResponse {
    let mut cur = serde_json::to_value(settings::current()).unwrap_or_default();
    if let (Some(c), Some(p)) = (cur.as_object_mut(), patch.as_object()) {
        for (k, v) in p {
            c.insert(k.clone(), v.clone());
        }
    }
    let Ok(next) = serde_json::from_value::<settings::Settings>(cur) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid settings"})));
    };
    if !list_models().iter().any(|m| m["file"] == next.model.as_str()) && next.model != settings::current().model {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "unknown model"})));
    }
    settings::save(next);
    (StatusCode::OK, Json(model_state(&app)))
}

async fn model_reset(State(app): State<Shared>) -> Json<serde_json::Value> {
    settings::save(settings::Settings::default());
    Json(model_state(&app))
}

/// Reload the engine with the saved model / batch size: stop the cameras, let the database flush, re-exec this binary.
/// The saved cameras come back by themselves once the new engine is ready.
async fn restart_api(State(app): State<Shared>) -> (StatusCode, Json<serde_json::Value>) {
    // find the program to relaunch BEFORE stopping anything. After a rebuild Linux reports the running file as "... (deleted)":
    // the new build lives at the same path.
    let exe = std::env::current_exe().ok().map(|p| {
        let t = p.to_string_lossy().to_string();
        std::path::PathBuf::from(t.strip_suffix(" (deleted)").unwrap_or(&t))
    });
    let Some(exe) = exe.filter(|p| p.exists()) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "cannot find the program to relaunch; restart Chira by hand"})));
    };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let cams: Vec<Arc<Camera>> = app.cams.lock().values().cloned().collect();
        for c in &cams {
            c.stop();
        }
        std::thread::sleep(std::time::Duration::from_millis(1000));
        use std::os::unix::process::CommandExt;
        let err = Command::new(&exe).args(std::env::args().skip(1)).exec();
        eprintln!("restart failed: {err}");
        std::process::exit(1);
    });
    (StatusCode::ACCEPTED, Json(json!({"ok": true})))
}

// ---------------------------------------------------------------- discovery
async fn discover_api(State(_): State<Shared>) -> Json<serde_json::Value> {
    let found = tokio::task::spawn_blocking(discover::discover).await.unwrap_or_default();
    Json(json!({"found": found}))
}

#[derive(Deserialize)]
struct StreamsReq {
    ip: String,
    #[serde(default)]
    onvif: String,
    #[serde(default)]
    user: String,
    #[serde(default)]
    pass: String,
}
async fn streams_api(State(app): State<Shared>, Json(r): Json<StreamsReq>) -> Json<serde_json::Value> {
    let list = tokio::task::spawn_blocking(move || discover::streams(&r.ip, &r.onvif, &r.user, &r.pass)).await.unwrap_or_default();
    let mut map = app.found.lock();
    let out: Vec<_> = list
        .into_iter()
        .map(|s| {
            let token = format!("{:x}", s.url.bytes().fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211)));
            map.insert(token.clone(), s.url.clone());
            json!({"token": token, "label": s.label, "url": s.shown, "detail": s.detail})
        })
        .collect();
    Json(json!({"streams": out}))
}

#[derive(Deserialize)]
struct CheckReq {
    #[serde(default)]
    url: String,
}
async fn check_api(Json(r): Json<CheckReq>) -> Json<serde_json::Value> {
    let url = r.url.trim().to_string();
    if !URL_SCHEMES.iter().any(|sc| url.starts_with(sc)) {
        return Json(json!({"ok": false, "error": "Use rtsp://, rtmp://, http(s):// or srt:// URL"}));
    }
    match tokio::task::spawn_blocking(move || discover::check(&url)).await.ok().flatten() {
        Some(d) => Json(json!({"ok": true, "detail": d})),
        None => Json(json!({"ok": false, "error": "No video found. Check the address, the credentials and that the camera allows RTSP."})),
    }
}

// ---------------------------------------------------------------- alarms
async fn incidents_api(State(app): State<Shared>) -> Json<serde_json::Value> {
    Json(json!({"now": analytics::now_ms(), "group_ms": analytics::GROUP_MS, "incidents": app.hub.incidents(500), "snoozes": app.hub.snoozes()}))
}

#[derive(Deserialize)]
struct AckReq {
    id: Option<u64>,
    cam: Option<String>,
}
async fn ack_api(State(app): State<Shared>, Json(r): Json<AckReq>) -> Json<serde_json::Value> {
    Json(json!({"acked": app.hub.ack(r.id, r.cam.as_deref())}))
}

#[derive(Deserialize)]
struct SnoozeReq {
    cam: String,
    rule_id: String,
    #[serde(default)]
    minutes: u64,
}
async fn snooze_api(State(app): State<Shared>, Json(r): Json<SnoozeReq>) -> StatusCode {
    if r.minutes == 0 {
        app.hub.unsnooze(&r.cam, &r.rule_id);
    } else {
        app.hub.snooze(&r.cam, &r.rule_id, r.minutes.min(60 * 24 * 7));
    }
    StatusCode::OK
}

async fn snapshot_api(Path(id): Path<u64>) -> impl IntoResponse {
    match tokio::fs::read(format!("{}/{id}.jpg", analytics::snap_dir())).await {
        Ok(b) => (StatusCode::OK, [("content-type", "image/jpeg"), ("cache-control", "private, max-age=86400")], b),
        Err(_) => (StatusCode::NOT_FOUND, [("content-type", "text/plain"), ("cache-control", "no-store")], Vec::new()),
    }
}

async fn stats(State(app): State<Shared>) -> Json<serde_json::Value> {
    let cams: Vec<_> = app
        .cams
        .lock()
        .values()
        .map(|c| {
            let s = &c.stats;
            json!({
                "id": c.id, "name": c.name, "setup": c.setup, "src_fps": c.fps.load(Relaxed), "kind": c.kind, "key": c.key,
                "status": *c.status.lock(),
                "fps": getf(&s.fps), "decode_fps": getf(&s.decode_fps), "display_fps": getf(&s.display_fps),
                "infer_ms": getf(&s.infer_ms), "lat_ms": getf(&s.lat_ms), "total_ms": getf(&s.total_ms),
                "tracks": getf(&s.tracks), "processed": s.processed.load(Relaxed),
                "dropped": s.dropped.load(Relaxed),
                "uptime": c.started.elapsed().as_secs_f64(),
            })
        })
        .collect();
    let g = *app.gpu.lock();
    let b = app.batcher.read().clone();
    let (avg, ms, cps) = b.as_ref().map(|b| (getf(&b.avg_batch), getf(&b.batch_ms), getf(&b.calls_per_s))).unwrap_or((0.0, 0.0, 0.0));
    Json(json!({
        "dev": app.dev,
        "engine": {
            "ready": b.is_some(), "error": *app.error.lock(), "message": detector::STATUS.lock().clone(),
            "ep": std::env::var("EP").unwrap_or_else(|_| "cuda".into()),
            "model": pipeline::model_path().rsplit('/').next().unwrap_or("").trim_end_matches(".onnx").to_string(),
            "w": detector::dims().w, "h": detector::dims().h,
        },
        "gpu": {"util": g.util, "mem_used": g.mem_used, "mem_total": g.mem_total, "power": g.power},
        "cpu_cores": *app.cpu_cores.lock(),
        "batch": {"max": app.max_batch, "avg": avg, "ms": ms, "calls_per_s": cps},
        "cameras": cams,
    }))
}

async fn ws(State(app): State<Shared>, Path(id): Path<u32>, up: WebSocketUpgrade) -> impl IntoResponse {
    let cam = app.cams.lock().get(&id).cloned();
    up.on_upgrade(move |sock| async move {
        if let Some(cam) = cam {
            pump(sock, cam).await
        }
    })
}

async fn pump(mut sock: WebSocket, cam: Arc<Camera>) {
    let mut rx = cam.tx.subscribe();
    while rx.changed().await.is_ok() {
        let msg = rx.borrow_and_update().clone();
        if msg.is_empty() {
            continue;
        }
        if sock.send(Message::Binary(msg.to_vec())).await.is_err() {
            break;
        }
    }
}

/// background sampler: nvidia-smi for the GPU, /proc/self/stat for this process' CPU use
fn spawn_sampler(app: Shared) {
    {
        let app = app.clone();
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let child = Command::new("nvidia-smi")
                .args([
                    "--query-gpu=utilization.gpu,memory.used,memory.total,power.draw",
                    "--format=csv,noheader,nounits",
                    "-lms",
                    "500",
                ])
                .stdout(Stdio::piped())
                .spawn();
            let Ok(mut child) = child else { return };
            for line in BufReader::new(child.stdout.take().unwrap()).lines().flatten() {
                let v: Vec<f64> = line.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if v.len() == 4 {
                    *app.gpu.lock() = Gpu { util: v[0], mem_used: v[1], mem_total: v[2], power: v[3] };
                }
            }
        });
    }
    std::thread::spawn(move || {
        // CPU of this process + its ffmpeg children, in "cores"
        let read = || -> f64 {
            let mut total = 0.0;
            let me = std::process::id();
            let tick = 100.0; // CLK_TCK
            let mut pids = vec![me];
            if let Ok(out) = Command::new("pgrep").args(["-P", &me.to_string()]).output() {
                for l in String::from_utf8_lossy(&out.stdout).lines() {
                    if let Ok(p) = l.trim().parse() {
                        pids.push(p);
                    }
                }
            }
            for p in pids {
                if let Ok(s) = std::fs::read_to_string(format!("/proc/{p}/stat")) {
                    if let Some(rest) = s.rsplit(')').next() {
                        let f: Vec<&str> = rest.split_whitespace().collect();
                        if f.len() > 13 {
                            let ut: f64 = f[11].parse().unwrap_or(0.0);
                            let st: f64 = f[12].parse().unwrap_or(0.0);
                            total += (ut + st) / tick;
                        }
                    }
                }
            }
            total
        };
        let mut prev = read();
        let mut t = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(1000));
            let cur = read();
            let dt = t.elapsed().as_secs_f64();
            // children that exited make `cur` drop; clamp
            *app.cpu_cores.lock() = ((cur - prev) / dt).max(0.0);
            prev = cur;
            t = std::time::Instant::now();
        }
    });
}

#[tokio::main]
async fn main() {
    if std::env::args().any(|a| a == "--bench-model") {
        let mut d = detector::Detector::new(&pipeline::model_path(), std::env::var("MAX_BATCH").ok().and_then(|v| v.parse().ok()).unwrap_or(8)).unwrap();
        for b in 1..=8usize {
            let x = vec![114u8; detector::dims().frame_bytes() * b];
            for _ in 0..3 {
                d.detect(&x, b).unwrap();
            }
            let t = std::time::Instant::now();
            for _ in 0..20 {
                d.detect(&x, b).unwrap();
            }
            let ms = t.elapsed().as_secs_f64() * 1000.0 / 20.0;
            println!("batch {b}: {ms:.1} ms/call  {:.1} ms/frame", ms / b as f64);
        }
        println!("-- alternating batch sizes");
        let pat = [1usize, 2, 3, 2, 1, 4, 3, 2];
        for _ in 0..2 {
            let t = std::time::Instant::now();
            for &b in pat.iter().cycle().take(40) {
                d.detect(&vec![114u8; detector::dims().frame_bytes() * b], b).unwrap();
            }
            println!("mixed shapes: {:.1} ms/call", t.elapsed().as_secs_f64() * 1000.0 / 40.0);
        }
        return;
    }
    let trt = std::env::var("EP").as_deref() == Ok("trt");
    settings::loaded_model();   // remember what this process loads
    let max_batch: usize = settings::effective_batch(&pipeline::model_path(), trt);
    let wait_ms: f64 = std::env::var("BATCH_WAIT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(40.0);
    let app: Shared = Arc::new(App {
        batcher: RwLock::new(None),
        error: Mutex::new(None),
        hub: Arc::new(analytics::Hub::new()),
        cams: Default::default(),
        next_id: Default::default(),
        gpu: Default::default(),
        cpu_cores: Default::default(),
        max_batch,
        dev: std::env::args().any(|a| a == "--dev") || std::env::var("CHIRA_DEV").map_or(false, |v| v == "1" || v == "true"),
        found: Default::default(),
    });
    // the UI is available immediately; the (possibly slow, first time) engine build runs in the background
    {
        let app = app.clone();
        std::thread::spawn(move || {
            println!("loading {} (max batch {max_batch}, batch wait {wait_ms} ms, {}) ...", pipeline::model_path(), if trt { "TensorRT" } else { "CUDA" });
            match batcher::Batcher::start(&pipeline::model_path(), max_batch, wait_ms) {
                Ok(b) => {
                    detector::set_status("Ready");
                    *app.batcher.write() = Some(b.clone());
                    println!("engine ready");
                    // bring back the cameras that were added before the last shutdown
                    for src in app.hub.sources() {
                        spawn_camera(&app, b.clone(), &src, false);
                    }
                }
                Err(e) => {
                    eprintln!("model load failed: {e:#}");
                    *app.error.lock() = Some(format!("{e}"));
                }
            }
        });
    }
    spawn_sampler(app.clone());
    let router = Router::new()
        .route("/", get(index))
        .route("/api/cameras", post(add_camera))
        .route("/api/cameras/:id", delete(remove_camera))
        .route("/api/stats", get(stats))
        .route("/api/model", get(model_get).put(model_put))
        .route("/api/model/reset", post(model_reset))
        .route("/api/restart", post(restart_api))
        .route("/api/config/:id", get(get_config).put(put_config))
        .route("/api/events", get(events))
        .route("/api/discover", post(discover_api))
        .route("/api/streams", post(streams_api))
        .route("/api/check", post(check_api))
        .route("/api/incidents", get(incidents_api))
        .route("/api/incidents/ack", post(ack_api))
        .route("/api/snooze", post(snooze_api))
        .route("/api/snapshot/:id", get(snapshot_api))
        .route("/ws/alerts", get(alerts_ws))
        .route("/s/fonts/:name", get(font))
        .route("/s/:name", get(asset))
        .route("/ws/:id", get(ws))
        .with_state(app.clone());
    let port = std::env::var("PORT").unwrap_or_else(|_| "8787".into());
    // local only by default: the page can add cameras (with credentials) and shows their snapshots. HOST=0.0.0.0 exposes it on the LAN.
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let addr = format!("{host}:{port}");
    println!("open http://localhost:{port}{}", if app.dev { "   (developer mode: performance statistics visible)" } else { "" });
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .unwrap();
    for c in app.cams.lock().values() {
        c.stop();
    }
}
