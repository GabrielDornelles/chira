//! Dynamic batching: ONE model, ONE GPU stream, N cameras.
//! Each camera keeps only its newest frame waiting (a real camera never waits for you). The batcher thread
//! takes every waiting frame (up to `max_batch`, oldest first), runs a single GPU call, and hands each
//! camera its own detections. Under light load batches are size 1; when the GPU is the bottleneck they
//! grow by themselves, which is exactly when batching pays off.

use crate::detector::{Detector, FRAME_BYTES};
use crate::pipeline::{set, Stats};
use crate::tracker::Det;
use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct Job {
    pub frame: Vec<u8>,
    pub captured: Instant,
}

pub struct Output {
    pub frame: Vec<u8>,
    pub captured: Instant,
    pub dets: Vec<Det>,
    pub infer_ms: f64,
    #[allow(dead_code)]
    pub batch: usize,
}

struct Entry {
    pending: Option<Job>,
    tx: Sender<Output>,
    stats: Arc<Stats>,
}

pub struct Batcher {
    state: Mutex<HashMap<u32, Entry>>,
    cv: Condvar,
    max_batch: usize,
    wait: Duration,
    pub avg_batch: AtomicU64,
    pub batch_ms: AtomicU64,
    pub calls_per_s: AtomicU64,
}

impl Batcher {
    /// Loads the model (blocking), then starts the batching thread.
    pub fn start(model: &str, max_batch: usize, wait_ms: f64) -> anyhow::Result<Arc<Self>> {
        let det = Detector::new(model, max_batch)?;
        let b = Arc::new(Self {
            state: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
            max_batch,
            wait: Duration::from_secs_f64(wait_ms / 1000.0),
            avg_batch: AtomicU64::new(0),
            batch_ms: AtomicU64::new(0),
            calls_per_s: AtomicU64::new(0),
        });
        let bb = b.clone();
        std::thread::Builder::new().name("batcher".into()).spawn(move || bb.run(det))?;
        Ok(b)
    }

    pub fn register(&self, cam: u32, stats: Arc<Stats>) -> Receiver<Output> {
        let (tx, rx) = channel();
        self.state.lock().insert(cam, Entry { pending: None, tx, stats });
        rx
    }

    pub fn unregister(&self, cam: u32) {
        self.state.lock().remove(&cam);
    }

    /// Replaces the camera's waiting frame (counting the replaced one as dropped).
    pub fn submit(&self, cam: u32, job: Job) {
        let mut st = self.state.lock();
        if let Some(e) = st.get_mut(&cam) {
            if e.pending.replace(job).is_some() {
                e.stats.dropped.fetch_add(1, Relaxed);
            }
        }
        drop(st);
        self.cv.notify_one();
    }

    fn run(self: Arc<Self>, mut det: Detector) {
        let mut input: Vec<u8> = Vec::new();
        let mut cur_n = 1usize; // the batch shape ORT is currently warm for
        let (mut calls, mut frames, mut ms_acc) = (0u64, 0u64, 0.0f64);
        let mut win = Instant::now();
        loop {
            let mut jobs: Vec<(u32, Job, Sender<Output>)> = Vec::new();
            let target;
            {
                let mut st = self.state.lock();
                // 1. wait for the first frame
                while !st.values().any(|e| e.pending.is_some()) {
                    self.cv.wait_for(&mut st, Duration::from_millis(100));
                }
                // 2. the batch shape is fixed to the number of cameras (capped), so wait (briefly) for all of them
                target = st.len().clamp(1, self.max_batch);
                if target > 1 {
                    let deadline = Instant::now() + self.wait;
                    while st.values().filter(|e| e.pending.is_some()).count() < target {
                        if self.cv.wait_until(&mut st, deadline).timed_out() {
                            break;
                        }
                    }
                }
                // 3. oldest frames first so no camera starves
                let mut ready: Vec<(u32, Instant)> =
                    st.iter().filter_map(|(id, e)| e.pending.as_ref().map(|j| (*id, j.captured))).collect();
                ready.sort_by_key(|r| r.1);
                for (id, _) in ready.into_iter().take(target) {
                    let e = st.get_mut(&id).unwrap();
                    jobs.push((id, e.pending.take().unwrap(), e.tx.clone()));
                }
            }
            // cameras were added/removed: warm the new shape once (frames arriving meanwhile replace each other)
            if target != cur_n {
                if let Err(e) = det.warm(target) {
                    eprintln!("warm-up failed: {e:#}");
                }
                cur_n = target;
            }
            // 4. one GPU call; slots without a fresh frame keep stale pixels (padding, result discarded)
            let n = jobs.len();
            input.resize(FRAME_BYTES * target, 114);
            for (i, (_, j, _)) in jobs.iter().enumerate() {
                input[i * FRAME_BYTES..(i + 1) * FRAME_BYTES].copy_from_slice(&j.frame);
            }
            let t0 = Instant::now();
            let res = det.detect(&input, target);
            let infer_ms = t0.elapsed().as_secs_f64() * 1000.0;
            match res {
                Ok(all) => {
                    for ((_, j, tx), dets) in jobs.into_iter().zip(all) {
                        let _ = tx.send(Output { frame: j.frame, captured: j.captured, dets, infer_ms, batch: n });
                    }
                }
                Err(e) => eprintln!("inference failed: {e:#}"),
            }
            calls += 1;
            frames += n as u64;
            ms_acc += infer_ms;
            if win.elapsed() >= Duration::from_millis(500) {
                let el = win.elapsed().as_secs_f64();
                set(&self.avg_batch, frames as f64 / calls as f64);
                set(&self.batch_ms, ms_acc / calls as f64);
                set(&self.calls_per_s, calls as f64 / el);
                calls = 0;
                frames = 0;
                ms_acc = 0.0;
                win = Instant::now();
            }
        }
    }
}
