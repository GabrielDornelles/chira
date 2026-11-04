//! One camera = 4 OS threads, no GIL, nothing shared between cameras except the GPU:
//!   publisher ffmpeg  -> tcp (mpegts) -> reader ffmpeg (decode+scale, rawvideo on a pipe)
//!   reader thread     -> frame queue
//!   inference thread  -> YOLOX (CUDA) + ByteTrack + line counter
//!   encoder thread    -> JPEG + metadata -> websocket subscribers

use crate::alarms::AlarmEngine;
use crate::analytics::{now_ms, Event, Hub, Occ};
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
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
    let rule = |id: &str, name: &str, kind: &str, target: &str, classes: &[&str], seconds, percent| AlarmRule {
        id: id.into(), name: name.into(), kind: kind.into(), target: target.into(), classes: classes.iter().map(|c| c.to_string()).collect(),
        seconds, percent, direction: "any".into(), enabled: true,
    };
    let classes: Vec<String> = ["car", "truck", "bus", "motorcycle"].iter().map(|c| c.to_string()).collect();
    if setup == 1 {
        CameraConfig {
            classes,
            lines: vec![line("l1", "Line A", 0.04, 0.62, 0.62, 0.62)],
            zones: vec![zone("z1", "Approach", &[[0.0, 0.66], [0.50, 0.66], [0.66, 1.0], [0.0, 1.0]], 8)],
            alarms: vec![
                rule("a1", "Heavy vehicle", "enter", "z1", &["truck", "bus"], 0.0, 80.0),
                rule("a2", "Slow vehicle", "dwell", "z1", &[], 10.0, 80.0),
                rule("a3", "Zone nearly full", "occupancy", "z1", &[], 0.0, 85.0),
            ],
        }
    } else {
        CameraConfig {
            classes,
            lines: vec![line("l1", "Line B", 0.25, 0.30, 0.56, 0.30), line("l2", "Line C", 0.0, 0.78, 0.66, 0.70)],
            zones: vec![zone("z1", "Merge", &[[0.22, 0.32], [0.56, 0.32], [0.64, 0.70], [0.02, 0.76]], 6)],
            alarms: vec![
                rule("a1", "Heavy vehicle", "enter", "z1", &["truck", "bus"], 0.0, 80.0),
                rule("a2", "Slow vehicle", "dwell", "z1", &[], 10.0, 80.0),
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

pub struct Camera {
    pub id: u32,
    pub name: String,
    pub setup: u32,
    pub fps: u32,
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

pub fn start(id: u32, setup: &Setup, fps: u32, port: u16, batcher: Arc<Batcher>, hub: Arc<Hub>) -> Arc<Camera> {
    let (tx, _rx) = watch::channel(Arc::new(Vec::new()));
    let cam = Arc::new(Camera {
        id,
        name: setup.name.to_string(),
        setup: setup.id,
        fps,
        status: Mutex::new("starting (loading model)".into()),
        stats: Arc::new(Stats::default()),
        stop: Arc::new(AtomicBool::new(false)),
        started: Instant::now(),
        config: RwLock::new(config::load(setup.id).unwrap_or_else(|| default_config(setup.id))),
        cfg_ver: AtomicU64::new(1),
        hub,
        tx,
        children: Mutex::new(Vec::new()),
    });
    let start_s = setup.start_s;
    let c = cam.clone();
    std::thread::Builder::new()
        .name(format!("cam{id}-main"))
        .spawn(move || {
            if let Err(e) = run(c.clone(), start_s, port, batcher) {
                eprintln!("camera {} failed: {e:#}", c.id);
                *c.status.lock() = format!("error: {e}");
                c.stop.store(true, Relaxed);
            }
        })
        .unwrap();
    cam
}

fn run(cam: Arc<Camera>, start_s: u32, port: u16, batcher: Arc<Batcher>) -> anyhow::Result<()> {
    let rx = batcher.register(cam.id, cam.stats.clone());
    let r = run_inner(&cam, start_s, port, &batcher, rx);
    batcher.unregister(cam.id);
    r
}

fn run_inner(
    cam: &Arc<Camera>,
    start_s: u32,
    port: u16,
    batcher: &Arc<Batcher>,
    rx: std::sync::mpsc::Receiver<crate::batcher::Output>,
) -> anyhow::Result<()> {

    // 2. "the stream": a publisher ffmpeg that serves the mp4 as MPEG-TS over TCP
    //    (-re = real time 30 fps, like a camera; without it, as fast as the consumer pulls)
    let url = format!("tcp://127.0.0.1:{port}");
    let reader = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-fflags", "nobuffer", "-f", "mpegts", "-i"])
        .arg(format!("{url}?listen=1"))
        .args([
            "-vf",
            &format!("scale={W}:{VIEW_H}:flags=fast_bilinear,pad={W}:{H}:0:0:color=0x727272"),
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .stdin(Stdio::null())
        .spawn()?;
    let mut reader = reader;
    let mut stdout = reader.stdout.take().unwrap();
    cam.children.lock().push(reader);
    std::thread::sleep(Duration::from_millis(400));
    // A real stream is paced by its source: `-re` makes ffmpeg emit frames at wall-clock rate.
    // The clip is 30 fps, so for 60/120 fps sources frames are duplicated (fps filter) and
    // re-encoded with low-latency x264, like a camera encoder would. Playback speed is never changed.
    let mut pubcmd = Command::new("ffmpeg");
    pubcmd.args(["-loglevel", "error", "-re", "-stream_loop", "-1", "-ss", &start_s.to_string(), "-i", &video_path(), "-an"]);
    if cam.fps == 30 {
        // native rate: just re-mux the camera's H.264 (still paced in real time by -re)
        pubcmd.args(["-c:v", "copy", "-bsf:v", "h264_mp4toannexb"]);
    } else {
        pubcmd
            .args(["-vf", &format!("fps={}", cam.fps)])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-b:v", "12M"])
            .args(["-g", &cam.fps.to_string()]);
    }
    pubcmd
        .args(["-f", "mpegts"])
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    cam.children.lock().push(pubcmd.spawn()?);

    // 3. reader thread (pipe -> queue)
    {
        let batcher = batcher.clone();
        let cam = cam.clone();
        std::thread::Builder::new().name(format!("cam{}-read", cam.id)).spawn(move || {
            loop {
                let mut buf = vec![0u8; FRAME_BYTES];
                if stdout.read_exact(&mut buf).is_err() {
                    break;
                }
                cam.stats.decoded.fetch_add(1, Relaxed);
                batcher.submit(cam.id, Job { frame: buf, captured: Instant::now() });
                if cam.stop.load(Relaxed) {
                    break;
                }
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
    *cam.status.lock() = "running".into();
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
        publish(&cam, &crossings, &zone_events, &fired);
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

/// Turn engine output into stored events (analytics) and live alerts (UI).
fn publish(cam: &Camera, crossings: &[crate::counter::CrossEv], zone_events: &[crate::zones::ZoneEv], fired: &[crate::alarms::Fired]) {
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
        cam.hub.push(e.clone());
        cam.hub.alert(&e);
    }
}
