//! YOLO26 / YOLOX inference through ONNX Runtime (TensorRT or CUDA execution provider), batched.
//! The ONNX graph itself does: uint8 NHWC RGB -> BGR NCHW fp16 -> YOLOX -> box decode -> class filter ->
//! top-300 per image, so the CPU only receives a tiny [B, 300, 6] tensor (x1 y1 x2 y2 score class).

use crate::tracker::Det;
use anyhow::Result;
use ort::execution_providers::{CUDAExecutionProvider, TensorRTExecutionProvider};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::TensorRef;

pub const W: usize = 960;
pub const H: usize = 544; // 540 video rows + 4 rows of padding (multiple of 32)
pub const FRAME_BYTES: usize = W * H * 3;

/// Human-readable engine state for the UI ("Compiling TensorRT engine 2/4 ...").
pub static STATUS: parking_lot::Mutex<String> = parking_lot::const_mutex(String::new());
pub fn set_status(m: impl Into<String>) {
    *STATUS.lock() = m.into();
}

pub struct Detector {
    session: Session,
}

impl Detector {
    /// `EP=trt` -> TensorRT (fp16, engines cached in ./trt_cache), default: CUDA EP.
    pub fn new(model: &str, max_batch: usize) -> Result<Self> {
        set_status("Loading model");
        let builder = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_memory_pattern(false)?
            .with_intra_threads(1)?;
        let session = if std::env::var("EP").as_deref() == Ok("trt") {
            std::fs::create_dir_all("trt_cache").ok();
            builder
                .with_execution_providers([
                    TensorRTExecutionProvider::default()
                        .with_device_id(0)
                        .with_fp16(true)
                        .with_engine_cache(true)
                        .with_engine_cache_path("trt_cache")
                        .with_timing_cache(true)
                        .with_timing_cache_path("trt_cache")
                        .with_max_workspace_size(4 << 30)
                        .build()
                        .error_on_failure(),
                    CUDAExecutionProvider::default().with_device_id(0).build(),
                ])?
                .commit_from_file(model)?
        } else {
            builder
                .with_execution_providers([CUDAExecutionProvider::default().with_device_id(0).build().error_on_failure()])?
                .commit_from_file(model)?
        };
        let mut d = Self { session };
        let trt = std::env::var("EP").as_deref() == Ok("trt");
        set_status(if trt { format!("Compiling TensorRT engine 1/{max_batch} (one-time, cached afterwards)") } else { "Warming up".into() });
        d.warm(1)?;
        if std::env::var("EP").as_deref() == Ok("trt") {
            // TensorRT builds (and disk-caches) one engine per new batch shape: do it all at startup, not when a camera is added
            for b in 2..=max_batch {
                eprintln!("TensorRT: preparing batch {b}/{max_batch} ...");
                set_status(format!("Compiling TensorRT engine {b}/{max_batch} (one-time, cached afterwards)"));
                d.warm(b)?;
            }
        }
        Ok(d)
    }

    /// First run of a new input shape is slow (cuDNN plans); changing shape between calls costs ~150 ms,
    /// so the batcher keeps the shape fixed and warms each new one here.
    pub fn warm(&mut self, b: usize) -> Result<()> {
        let blank = vec![114u8; FRAME_BYTES * b];
        for _ in 0..3 {
            self.detect(&blank, b)?;
        }
        Ok(())
    }

    /// `rgb`: `b` frames, each W*H*3 interleaved RGB, back to back.
    /// Returns, per frame, detections (score >= 0.1) in frame pixels after NMS.
    pub fn detect(&mut self, rgb: &[u8], b: usize) -> Result<Vec<Vec<Det>>> {
        let tensor = TensorRef::from_array_view(([b, H, W, 3], rgb))?;
        let outputs = self.session.run(ort::inputs!["images" => tensor])?;
        let (shape, data) = outputs["output"].try_extract_tensor::<f32>()?;
        let k = shape[1] as usize;
        let mut all = Vec::with_capacity(b);
        for i in 0..b {
            let mut cands: Vec<Det> = Vec::new();
            for r in 0..k {
                let row = &data[(i * k + r) * 6..(i * k + r + 1) * 6];
                if row[4] < 0.1 {
                    break; // sorted by score, descending
                }
                cands.push(Det { x1: row[0], y1: row[1], x2: row[2], y2: row[3], score: row[4], cls: row[5] as u8 });
            }
            all.push(nms(cands, 0.45));
        }
        Ok(all)
    }
}

fn nms(dets: Vec<Det>, thr: f32) -> Vec<Det> {
    let mut keep: Vec<Det> = Vec::new();
    'outer: for d in dets {
        for k in &keep {
            if k.cls != d.cls {
                continue;
            }
            let ix = (d.x2.min(k.x2) - d.x1.max(k.x1)).max(0.0);
            let iy = (d.y2.min(k.y2) - d.y1.max(k.y1)).max(0.0);
            let inter = ix * iy;
            let u = (d.x2 - d.x1) * (d.y2 - d.y1) + (k.x2 - k.x1) * (k.y2 - k.y1) - inter;
            if u > 0.0 && inter / u > thr {
                continue 'outer;
            }
        }
        keep.push(d);
    }
    keep
}
