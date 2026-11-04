//! One camera = 4 OS threads, no GIL, nothing shared between cameras except the GPU:
//!   publisher ffmpeg  -> tcp (mpegts) -> reader ffmpeg (decode+scale, rawvideo on a pipe)
//!   reader thread     -> frame queue
//!   inference thread  -> YOLOX (CUDA) + ByteTrack + line counter
//!   encoder thread    -> JPEG + metadata -> websocket subscribers

use crate::alarms::AlarmEngine;
use crate::analytics::{now_ms, Event, Fire, Hub, Occ};
use crate::config::{self, class_name, AlarmRule, CameraConfig, LineCfg, ZoneCfg};
use crate::counter::LineCounter;
use crate::zones::{ZoneEngine, ZoneEvKind, ZoneSnap};
use crate::batcher::{Batcher, Job};
use crate::detector::{FRAME_BYTES, H, W};
use crate::tracker::ByteTracker;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Condvar};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// The simulated camera source (any video file; it is streamed in real time, in a loop). `VIDEO=path ./run.sh`
pub fn video_path() -> String {
    std::env::var("VIDEO").unwrap_or_else(|_| "videos/demo.mp4".into())
}
pub fn model_path() -> String {
    std::env::var("MODEL").unwrap_or_else(|_| "models/yolo26m_544x960.onnx".into())
}
pub const VIEW_H: usize = 540;

pub struct Setup {
    pub id: u32,
    pub name: &'static str,
    pub start_s: u32,
}

pub fn setups() -> Vec<Setup> {
    vec![Setup { id: 1, name: "Highway · North", start_s: 0 }, Setup { id: 2, name: "Highway · South", start_s: 95 }]
}

/// Out-of-the-box configuration of each preset (used until the user saves their own).
pub fn default_config(setup: u32) -> CameraConfig {
    let line = |id: &str, name: &str, x1, y1, x2, y2| LineCfg { id: id.into(), name: name.into(), x1, y1, x2, y2 };
    let zone = |id: &str, name: &str, pts: &[[f32; 2]], cap| ZoneCfg { id: id.into(), name: name.into(), points: pts.to_vec(), capacity: cap, classes: vec![] };
    let rule = |id: &str, name: &str, kind: &str, target: &str, classes: &[&str], seconds, percent, sev: &str| AlarmRule {
        id: id.into(), name: name.into(), kind: kind.into(), target: target.into(), classes: classes.iter().map(|c| c.to_string()).collect(),
        seconds, percent, direction: "any".into(), enabled: true, severity: sev.into(),
    };
    let classes: Vec<String> = ["car", "truck", "bus", "motorcycle"].iter().map(|c| c.to_string()).collect();
    if setup == 1 {
        CameraConfig {
            classes,
            lines: vec![line("l1", "Line A", 0.04, 0.62, 0.62, 0.62)],
            zones: vec![zone("z1", "Approach", &[[0.0, 0.66], [0.50, 0.66], [0.66, 1.0], [0.0, 1.0]], 8)],
            alarms: vec![
                rule("a1", "Heavy vehicle", "enter", "z1", &["truck", "bus"], 0.0, 80.0, "info"),
                rule("a2", "Slow vehicle", "dwell", "z1", &[], 10.0, 80.0, "warning"),
                rule("a3", "Zone nearly full", "occupancy", "z1", &[], 0.0, 85.0, "critical"),
            ],
        }
    } else {
        CameraConfig {
            classes,
            lines: vec![line("l1", "Line B", 0.25, 0.30, 0.56, 0.30), line("l2", "Line C", 0.0, 0.78, 0.66, 0.70)],
            zones: vec![zone("z1", "Merge", &[[0.22, 0.32], [0.56, 0.32], [0.64, 0.70], [0.02, 0.76]], 6)],
            alarms: vec![
                rule("a1", "Heavy vehicle", "enter", "z1", &["truck", "bus"], 0.0, 80.0, "info"),
                rule("a2", "Slow vehicle", "dwell", "z1", &[], 10.0, 80.0, "warning"),
            ],
        }
    }
}

// ------------------------------- stats --------------------------------------
#[derive(Default)]
pub struct Stats {
    pub decoded: AtomicU64,
    pub processed: AtomicU64,
    pub dropped: AtomicU64,
    pub displayed: AtomicU64,
    // f64 bit patterns
    pub fps: AtomicU64,
    pub decode_fps: AtomicU64,
    pub display_fps: AtomicU64,
    pub infer_ms: AtomicU64,
    pub total_ms: AtomicU64,
    pub tracks: AtomicU64,
    pub lat_ms: AtomicU64,
}
pub fn get(a: &AtomicU64) -> f64 {
    f64::from_bits(a.load(Relaxed))
}
pub fn set(a: &AtomicU64, v: f64) {
    a.store(v.to_bits(), Relaxed)
}

// ------------------------------- camera -------------------------------------
#[derive(Clone, Serialize)]
pub struct Meta {
    pub frame: u64,
    pub w: usize,
    pub h: usize,
    pub boxes: Vec<BoxOut>,
    pub lines: Vec<LineCfg>,
    pub counts: Vec<crate::counter::LineCounts>,
    pub zones: Vec<ZoneSnap>,
}
#[derive(Clone, Serialize)]
pub struct BoxOut {
    pub id: u32,
    pub b: [f32; 4],
    pub c: &'static str,
    pub s: f32,
}

pub enum Src {
    /// the bundled clip, published as a real-time MPEG-TS stream over local TCP
    Demo { setup: u32, start_s: u32, port: u16 },
    /// any URL ffmpeg can read: rtsp://, rtsps://, rtmp://, http(s) mjpeg/hls, srt://, udp:// ...
    Url(String),
}

pub struct Camera {
    pub id: u32,
    pub name: String,
    /// configuration key: lines, zones and alarms are saved per key
    pub key: String,
    pub setup: u32,
    /// source frame rate (configured for the demo, measured for real streams)
    pub fps: AtomicU32,
    pub kind: &'static str,
    pub status: Mutex<String>,
    pub stats: Arc<Stats>,
    pub stop: Arc<AtomicBool>,
    pub started: Instant,
    pub config: RwLock<CameraConfig>,
    pub cfg_ver: AtomicU64,
    pub hub: Arc<Hub>,
    pub tx: watch::Sender<Arc<Vec<u8>>>,
    children: Mutex<Vec<Child>>,
}

impl Camera {
    pub fn stop(&self) {
        self.stop.store(true, Relaxed);
        for c in self.children.lock().iter_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        *self.status.lock() = "stopped".into();
    }
}

/// Hide `user:pass@` of a stream URL (for the UI and logs).
pub fn redact(url: &str) -> String {
    match (url.find("://"), url.rfind('@')) {
        (Some(i), Some(j)) if j > i => format!("{}***@{}", &url[..i + 3], &url[j + 1..]),
        _ => url.to_string(),
    }
}

pub fn start(id: u32, name: String, key: String, src: Src, fps: u32, batcher: Arc<Batcher>, hub: Arc<Hub>) -> Arc<Camera> {
    let (tx, _rx) = watch::channel(Arc::new(Vec::new()));
    let (kind, setup) = match &src {
        Src::Demo { setup, .. } => ("demo", *setup),
        Src::Url(_) => ("url", 0),
    };
    let cam = Arc::new(Camera {
        id,
        name,
        config: RwLock::new(config::load(&key).unwrap_or_else(|| if setup > 0 { default_config(setup) } else { default_url_config() })),
        key,
        setup,
        fps: AtomicU32::new(fps),
        kind,
        status: Mutex::new("connecting".into()),
        stats: Arc::new(Stats::default()),
        stop: Arc::new(AtomicBool::new(false)),
        started: Instant::now(),
        cfg_ver: AtomicU64::new(1),
        hub,
        tx,
        children: Mutex::new(Vec::new()),
    });
    let c = cam.clone();
    std::thread::Builder::new()
        .name(format!("cam{id}-main"))
        .spawn(move || {
            if let Err(e) = run(c.clone(), src, batcher) {
                eprintln!("camera {} failed: {e:#}", c.id);
                *c.status.lock() = format!("error: {e}");
                c.stop.store(true, Relaxed);
            }
        })
        .unwrap();
    cam
}

/// A new real camera starts empty: the person draws their own lines and zones.
pub fn default_url_config() -> CameraConfig {
    CameraConfig { classes: ["car", "truck", "bus", "motorcycle"].iter().map(|c| c.to_string()).collect(), lines: vec![], zones: vec![], alarms: vec![] }
}

fn run(cam: Arc<Camera>, src: Src, batcher: Arc<Batcher>) -> anyhow::Result<()> {
    let rx = batcher.register(cam.id, cam.stats.clone());
    let r = run_inner(&cam, src, &batcher, rx);
    batcher.unregister(cam.id);
    r
}

/// decode + scale to 960x540 (letterboxed if the camera is not 16:9) + pad to 544 rows, raw RGB on a pipe
fn spawn_reader(input: &[String]) -> std::io::Result<Child> {
    Command::new("ffmpeg")
        .args(["-loglevel", "error", "-nostdin"])
        .args(input)
        .args([
            "-an",
            "-vf",
            &format!("scale={W}:{VIEW_H}:flags=fast_bilinear:force_original_aspect_ratio=decrease,pad={W}:{H}:(ow-iw)/2:(oh-ih)/2:color=0x727272"),
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .stdin(Stdio::null())
        .spawn()
}

fn url_input(url: &str) -> Vec<String> {
    let mut v: Vec<String> = ["-fflags", "nobuffer", "-flags", "low_delay"].iter().map(|s| s.to_string()).collect();
    if url.starts_with("rtsp") {
        // TCP is the safe default for cameras behind Wi-Fi / NAT; the socket timeout makes a dead camera fail fast
        v.extend(["-rtsp_transport", "tcp", crate::discover::rtsp_timeout_opt(), "8000000"].iter().map(|s| s.to_string()));
    } else if url.starts_with("rtmp") || url.starts_with("http") {
        v.extend(["-rw_timeout", "8000000"].iter().map(|s| s.to_string()));
    }
    v.extend(["-i".to_string(), url.to_string()]);
    v
}

fn run_inner(cam: &Arc<Camera>, src: Src, batcher: &Arc<Batcher>, rx: std::sync::mpsc::Receiver<crate::batcher::Output>) -> anyhow::Result<()> {
    // 2. the stream. Demo: a publisher ffmpeg serves the mp4 as MPEG-TS over TCP, paced in real time (-re) like a camera.
    //    Real camera: the reader pulls the URL itself and reconnects when it drops.
    //    A real stream is paced by its source; playback speed is never changed. For 60/120 fps demo sources frames
    //    are duplicated (fps filter) and re-encoded with low-latency x264, like a camera encoder would.
    if let Src::Demo { start_s, port, .. } = &src {
        let url = format!("tcp://127.0.0.1:{port}");
        let demo_in: Vec<String> = ["-fflags", "nobuffer", "-f", "mpegts", "-i"].iter().map(|s| s.to_string()).chain([format!("{url}?listen=1")]).collect();
        let fps = cam.fps.load(Relaxed);
        let mut pubcmd = Command::new("ffmpeg");
        pubcmd.args(["-loglevel", "error", "-re", "-stream_loop", "-1", "-ss", &start_s.to_string(), "-i", &video_path(), "-an"]);
        if fps == 30 {
            pubcmd.args(["-c:v", "copy", "-bsf:v", "h264_mp4toannexb"]);
        } else {
            pubcmd
                .args(["-vf", &format!("fps={fps}")])
                .args(["-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-b:v", "12M"])
                .args(["-g", &fps.to_string()]);
        }
        pubcmd.args(["-f", "mpegts"]).arg(&url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::inherit());
        // the reader must be listening before the publisher connects; the reader thread spawns it, then we start publishing
        let r = spawn_reader(&demo_in)?;
        cam.children.lock().push(r);
        std::thread::sleep(Duration::from_millis(400));
        cam.children.lock().push(pubcmd.spawn()?);
    }

    // 3. reader thread (pipe -> batcher), supervising the ffmpeg process
    {
        let batcher = batcher.clone();
        let cam = cam.clone();
        let url = if let Src::Url(u) = &src { Some(u.clone()) } else { None };
        std::thread::Builder::new().name(format!("cam{}-read", cam.id)).spawn(move || {
            let mut backoff = 1u64;
            let mut first = true;
            'outer: loop {
                // demo: the already-spawned reader (last-but-one child); url: spawn a fresh one
                let mut stdout = if let Some(u) = &url {
                    match spawn_reader(&url_input(u)) {
                        Ok(mut c) => {
                            let o = c.stdout.take().unwrap();
                            {
                                let mut ch = cam.children.lock();
                                ch.retain_mut(|x| x.try_wait().map(|s| s.is_none()).unwrap_or(false));
                                ch.push(c);
                            }
                            o
                        }
                        Err(e) => {
                            *cam.status.lock() = format!("error: {e}");
                            break;
                        }
                    }
                } else {
                    let mut ch = cam.children.lock();
                    ch[0].stdout.take().unwrap()
                };
                if cam.stop.load(Relaxed) {
                    break;
                }
                if !first {
                    *cam.status.lock() = "reconnecting".into();
                }
                first = false;
                let mut got_frame = false;
                loop {
                    let mut buf = vec![0u8; FRAME_BYTES];
                    if stdout.read_exact(&mut buf).is_err() {
                        break;
                    }
                    if !got_frame {
                        got_frame = true;
                        backoff = 1;
                        *cam.status.lock() = "running".into();
                    }
                    cam.stats.decoded.fetch_add(1, Relaxed);
                    batcher.submit(cam.id, Job { frame: buf, captured: Instant::now() });
                    if cam.stop.load(Relaxed) {
                        break 'outer;
                    }
                }
                if cam.stop.load(Relaxed) || url.is_none() {
                    break;
                }
                // the camera went away: wait (1, 2, 4 ... 10 s) and connect again
                *cam.status.lock() = "reconnecting".into();
                for _ in 0..backoff * 10 {
                    if cam.stop.load(Relaxed) {
                        break 'outer;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                backoff = (backoff * 2).min(10);
            }
            if !cam.stop.load(Relaxed) {
                *cam.status.lock() = "error: stream ended".into();
            }
            cam.stop.store(true, Relaxed);
        })?;
    }

    // 4. encoder thread: always encodes only the *latest* result, capped at 30 fps,
    //    so JPEG/websocket work can never slow inference down
    let slot: Arc<(std::sync::Mutex<Option<(Vec<u8>, Meta)>>, Condvar)> = Arc::new(Default::default());
    {
        let slot = slot.clone();
        let cam = cam.clone();
        std::thread::Builder::new().name(format!("cam{}-enc", cam.id)).spawn(move || {
            let mut last = Instant::now();
            let mut n = 0u64;
            let mut win = Instant::now();
            while !cam.stop.load(Relaxed) {
                let item = {
                    let mut g = slot.0.lock().unwrap();
                    while g.is_none() && !cam.stop.load(Relaxed) {
                        g = slot.1.wait_timeout(g, Duration::from_millis(50)).unwrap().0;
                    }
                    g.take()
                };
                let Some((rgb, meta)) = item else { continue };
                let min = Duration::from_millis(33);
                if last.elapsed() < min {
                    std::thread::sleep(min - last.elapsed());
                }
                last = Instant::now();
                let mut jpg = Vec::with_capacity(200_000);
                let enc = jpeg_encoder::Encoder::new(&mut jpg, 82);
                if enc.encode(&rgb[..W * VIEW_H * 3], W as u16, VIEW_H as u16, jpeg_encoder::ColorType::Rgb).is_err() {
                    continue;
                }
                let json = serde_json::to_vec(&meta).unwrap();
                let mut msg = Vec::with_capacity(4 + json.len() + jpg.len());
                msg.extend_from_slice(&(json.len() as u32).to_be_bytes());
                msg.extend_from_slice(&json);
                msg.extend_from_slice(&jpg);
                let _ = cam.tx.send(Arc::new(msg));
                cam.stats.displayed.fetch_add(1, Relaxed);
                n += 1;
                if win.elapsed() >= Duration::from_millis(500) {
                    set(&cam.stats.display_fps, n as f64 / win.elapsed().as_secs_f64());
                    n = 0;
                    win = Instant::now();
                }
            }
        })?;
    }

    // 5. inference loop (this thread)
    let cam = cam.clone();
    let mut tracker = ByteTracker::new();
    let mut counter = LineCounter::new(vec![], W as f32, VIEW_H as f32);
    let mut zones = ZoneEngine::new(&[], W as f32, VIEW_H as f32);
    let mut alarms = AlarmEngine::default();
    let mut allowed = [true; 256];
    let mut seen_ver = 0u64;
    let mut last_occ = Instant::now() - Duration::from_secs(10);
    let (mut n, mut dec0) = (0u64, 0u64);
    let (mut inf_acc, mut tot_acc, mut lat_acc) = (0.0f64, 0.0f64, 0.0f64);
    let mut win = Instant::now();
    let mut frame_no = 0u64;
    while !cam.stop.load(Relaxed) {
        let out = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let (frame, captured, mut dets) = (out.frame, out.captured, out.dets);
        let t0 = Instant::now();
        let t1 = t0;
        // live config changes (editor): applied between frames, counts of unchanged lines are kept
        let ver = cam.cfg_ver.load(Relaxed);
        if ver != seen_ver {
            let cfg = cam.config.read().clone();
            counter.reconfigure(cfg.lines.clone());
            zones.reconfigure(&cfg.zones);
            alarms.reconfigure(&cfg);
            allowed = cfg.allowed_class_ids();
            seen_ver = ver;
        }
        dets.retain(|d| allowed[d.cls as usize]);
        let tracks = tracker.update(&dets);
        let crossings = counter.update(&tracks);
        let now = Instant::now();
        let zone_events = zones.update(&tracks, now);
        let fired = alarms.eval(&zone_events, &crossings, &zones, now);
        publish(&cam, &frame, &tracks, &crossings, &zone_events, &fired);
        if last_occ.elapsed() >= Duration::from_secs(2) {
            last_occ = Instant::now();
            for (_, name, n, cap) in zones.occupancy() {
                cam.hub.push_occ(Occ { t: now_ms(), cam: cam.name.clone(), zone: name.to_string(), n, cap });
            }
        }
        frame_no += 1;
        let t2 = Instant::now();
        inf_acc += out.infer_ms;
        tot_acc += (t2 - t1).as_secs_f64() * 1000.0 + out.infer_ms;
        lat_acc += captured.elapsed().as_secs_f64() * 1000.0;
        n += 1;
        cam.stats.processed.fetch_add(1, Relaxed);
        set(&cam.stats.tracks, tracks.len() as f64);

        // hand the frame to the encoder (overwrites an unsent one)
        let meta = Meta {
            frame: frame_no,
            w: W,
            h: VIEW_H,
            boxes: tracks
                .iter()
                .map(|t| BoxOut { id: t.id, b: t.bbox, c: class_name(t.cls), s: t.score })
                .collect(),
            lines: counter.lines().to_vec(),
            counts: counter.counts().to_vec(),
            zones: zones.snapshot(),
        };
        *slot.0.lock().unwrap() = Some((frame, meta));
        slot.1.notify_one();

        if win.elapsed() >= Duration::from_millis(500) {
            let el = win.elapsed().as_secs_f64();
            set(&cam.stats.fps, n as f64 / el);
            let d = cam.stats.decoded.load(Relaxed);
            set(&cam.stats.decode_fps, (d - dec0) as f64 / el);
            if cam.kind == "url" && (d - dec0) > 5 {
                cam.fps.store(((d - dec0) as f64 / el).round() as u32, Relaxed);
            }
            dec0 = d;
            set(&cam.stats.infer_ms, inf_acc / n as f64);
            set(&cam.stats.total_ms, tot_acc / n as f64);
            set(&cam.stats.lat_ms, lat_acc / n as f64);
            lat_acc = 0.0;
            n = 0;
            inf_acc = 0.0;
            tot_acc = 0.0;
            win = Instant::now();
        }
    }
    Ok(())
}

/// Turn engine output into stored events (analytics) and alarm incidents (UI).
fn publish(cam: &Camera, frame: &[u8], tracks: &[crate::tracker::TrackOut], crossings: &[crate::counter::CrossEv], zone_events: &[crate::zones::ZoneEv], fired: &[crate::alarms::Fired]) {
    let t = now_ms();
    let base = |kind: &str, target: &str, cls: &str, track: u32| Event {
        t, cam: cam.name.clone(), setup: cam.setup, kind: kind.into(), target: target.into(), cls: cls.into(),
        dir: String::new(), dwell: 0.0, rule: String::new(), msg: String::new(), track,
    };
    for c in crossings {
        let mut e = base("cross", &c.line_name, class_name(c.cls), c.track);
        e.dir = if c.fwd { "fwd".into() } else { "rev".into() };
        cam.hub.push(e);
    }
    for z in zone_events {
        match z.kind {
            ZoneEvKind::Enter => cam.hub.push(base("enter", &z.zone_name, class_name(z.cls), z.track)),
            ZoneEvKind::Exit { dwell_s } => {
                let mut e = base("exit", &z.zone_name, class_name(z.cls), z.track);
                e.dwell = dwell_s;
                cam.hub.push(e);
            }
        }
    }
    for f in fired {
        let mut e = base("alarm", &f.target_name, &f.cls, f.track);
        e.rule = f.rule_name.clone();
        e.msg = f.msg.clone();
        e.dir = f.kind.clone();
        cam.hub.push(e);
        let bbox = tracks.iter().find(|t| t.id == f.track && f.track != 0).map(|t| t.bbox);
        cam.hub.fire(
            Fire {
                cam: cam.name.clone(), rule_id: f.rule_id.clone(), rule: f.rule_name.clone(), kind: f.kind.clone(), severity: f.severity.clone(),
                target: f.target_name.clone(), cls: f.cls.clone(), msg: f.msg.clone(), bbox,
            },
            || {
                // a snapshot is only taken for the first firing of an incident
                let mut jpg = Vec::with_capacity(120_000);
                jpeg_encoder::Encoder::new(&mut jpg, 80).encode(&frame[..W * VIEW_H * 3], W as u16, VIEW_H as u16, jpeg_encoder::ColorType::Rgb).ok()?;
                Some(jpg)
            },
        );
    }
}
