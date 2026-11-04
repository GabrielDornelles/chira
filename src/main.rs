mod alarms;
mod analytics;
mod batcher;
mod config;
mod counter;
mod detector;
mod pipeline;
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
}
type Shared = Arc<App>;

#[derive(Deserialize)]
struct AddReq {
    setup: u32,
    #[serde(default = "default_fps")]
    fps: u32,
}

fn default_fps() -> u32 {
    30
}

/// Served from disk when present (edit + refresh while developing), embedded copy otherwise.
async fn index() -> impl IntoResponse {
    let body = tokio::fs::read_to_string("static/index.html").await.unwrap_or_else(|_| include_str!("../static/index.html").to_string());
    ([("content-type", "text/html; charset=utf-8"), ("cache-control", "no-store")], body)
}

const ASSETS: [(&str, &str); 4] = [
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

async fn get_config(State(app): State<Shared>, Path(id): Path<u32>) -> impl IntoResponse {
    let Some(cam) = app.cams.lock().get(&id).cloned() else { return (StatusCode::NOT_FOUND, Json(json!({}))) };
    let cfg = cam.config.read().clone();
    (StatusCode::OK, Json(json!({"setup": cam.setup, "name": cam.name, "classes": config::CLASSES.iter().map(|c| c.0).collect::<Vec<_>>(), "config": cfg})))
}

async fn put_config(State(app): State<Shared>, Path(id): Path<u32>, Json(mut cfg): Json<config::CameraConfig>) -> impl IntoResponse {
    let Some(cam) = app.cams.lock().get(&id).cloned() else { return (StatusCode::NOT_FOUND, Json(json!({}))) };
    cfg.sanitize();
    config::save(cam.setup, &cfg);
    // every running camera of the same preset gets it
    for c in app.cams.lock().values().filter(|c| c.setup == cam.setup) {
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

async fn add_camera(State(app): State<Shared>, Json(req): Json<AddReq>) -> impl IntoResponse {
    let Some(batcher) = app.batcher.read().clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "engine is still starting"})));
    };
    let setups = pipeline::setups();
    let Some(setup) = setups.iter().find(|s| s.id == req.setup) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "unknown setup"})));
    };
    let id = app.next_id.fetch_add(1, Relaxed) + 1;
    let cam = pipeline::start(id, setup, req.fps.clamp(1, 240), 9100 + id as u16, batcher, app.hub.clone());
    app.cams.lock().insert(id, cam);
    (StatusCode::OK, Json(json!({"id": id})))
}

async fn remove_camera(State(app): State<Shared>, Path(id): Path<u32>) -> StatusCode {
    let cam = app.cams.lock().remove(&id);
    match cam {
        Some(c) => {
            tokio::task::spawn_blocking(move || c.stop());
            StatusCode::OK
        }
        None => StatusCode::NOT_FOUND,
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
                "id": c.id, "name": c.name, "setup": c.setup, "src_fps": c.fps,
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
        "engine": {
            "ready": b.is_some(), "error": *app.error.lock(), "message": detector::STATUS.lock().clone(),
            "ep": std::env::var("EP").unwrap_or_else(|_| "cuda".into()),
            "model": pipeline::model_path().rsplit('/').next().unwrap_or("").trim_end_matches(".onnx").to_string(),
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
            let x = vec![114u8; detector::FRAME_BYTES * b];
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
                d.detect(&vec![114u8; detector::FRAME_BYTES * b], b).unwrap();
            }
            println!("mixed shapes: {:.1} ms/call", t.elapsed().as_secs_f64() * 1000.0 / 40.0);
        }
        return;
    }
    let trt = std::env::var("EP").as_deref() == Ok("trt");
    let max_batch: usize = std::env::var("MAX_BATCH").ok().and_then(|v| v.parse().ok()).unwrap_or(if trt { 4 } else { 8 });
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
    });
    // the UI is available immediately; the (possibly slow, first time) engine build runs in the background
    {
        let app = app.clone();
        std::thread::spawn(move || {
            println!("loading {} (max batch {max_batch}, batch wait {wait_ms} ms, {}) ...", pipeline::model_path(), if trt { "TensorRT" } else { "CUDA" });
            match batcher::Batcher::start(&pipeline::model_path(), max_batch, wait_ms) {
                Ok(b) => {
                    detector::set_status("Ready");
                    *app.batcher.write() = Some(b);
                    println!("engine ready");
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
        .route("/api/config/:id", get(get_config).put(put_config))
        .route("/api/events", get(events))
        .route("/ws/alerts", get(alerts_ws))
        .route("/s/:name", get(asset))
        .route("/ws/:id", get(ws))
        .with_state(app.clone());
    let port = std::env::var("PORT").unwrap_or_else(|_| "8787".into());
    let addr = format!("0.0.0.0:{port}");
    println!("open http://localhost:{port}");
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
