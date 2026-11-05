//! Detection settings that belong to the whole engine rather than one camera: which model (= which processing
//! resolution), how confident a detection must be, how aggressively overlapping boxes are merged, how many cameras share
//! a GPU batch, and which object classes a new camera starts with. Stored in `data/model.json`.
//!
//! Confidence and overlap apply live. The model and the batch size need the engine to be reloaded (the UI offers a restart).

use crate::config::{data_dir, CLASSES};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

pub const DEFAULT_MODEL: &str = "models/yolo26m_544x960.onnx";
pub const DEFAULT_CONFIDENCE: f32 = 0.5;
pub const DEFAULT_OVERLAP: f32 = 0.45;
pub const DEFAULT_NEW_CAMERA_CLASSES: [&str; 8] = ["person", "car", "truck", "bus", "motorcycle", "bicycle", "dog", "cat"];

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default)]
pub struct Settings {
    /// ONNX file; the processing resolution is whatever that file was exported at (see `parse_res`).
    pub model: String,
    /// Minimum confidence for a detection to start tracking an object (weaker ones can still extend a track that exists).
    pub confidence: f32,
    /// Boxes of the same class that overlap more than this are treated as one object (NMS IoU).
    pub overlap: f32,
    /// Cameras per GPU batch. 0 = automatic (depends on the resolution).
    pub max_batch: u32,
    /// Classes a newly added camera tracks.
    pub default_classes: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.into(),
            confidence: DEFAULT_CONFIDENCE,
            overlap: DEFAULT_OVERLAP,
            max_batch: 0,
            default_classes: DEFAULT_NEW_CAMERA_CLASSES.iter().map(|c| c.to_string()).collect(),
        }
    }
}

impl Settings {
    /// Clamp / repair user input.
    pub fn sanitize(&mut self) {
        let d = Settings::default();
        self.confidence = if self.confidence.is_finite() { self.confidence.clamp(0.1, 0.95) } else { d.confidence };
        self.overlap = if self.overlap.is_finite() { self.overlap.clamp(0.1, 0.9) } else { d.overlap };
        self.max_batch = self.max_batch.min(16);
        self.default_classes.retain(|c| CLASSES.iter().any(|(n, _)| n == c));
        self.default_classes.dedup();
        if self.default_classes.is_empty() {
            self.default_classes = d.default_classes;
        }
        if self.model.trim().is_empty() {
            self.model = d.model;
        }
    }
}

static CURRENT: RwLock<Option<Settings>> = parking_lot::const_rwlock(None);
static CONF_BITS: AtomicU32 = AtomicU32::new(0);
static OVERLAP_BITS: AtomicU32 = AtomicU32::new(0);

fn path() -> String {
    format!("{}/model.json", data_dir())
}

pub fn current() -> Settings {
    if let Some(s) = CURRENT.read().as_ref() {
        return s.clone();
    }
    let mut s: Settings = std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    s.sanitize();
    apply_live(&s);
    *CURRENT.write() = Some(s.clone());
    s
}

fn apply_live(s: &Settings) {
    CONF_BITS.store(s.confidence.to_bits(), Relaxed);
    OVERLAP_BITS.store(s.overlap.to_bits(), Relaxed);
}

/// Validate, persist and apply (confidence / overlap take effect on the next frame).
pub fn save(mut s: Settings) -> Settings {
    s.sanitize();
    std::fs::create_dir_all(data_dir()).ok();
    if let Ok(t) = serde_json::to_string_pretty(&s) {
        let _ = std::fs::write(path(), t);
    }
    apply_live(&s);
    *CURRENT.write() = Some(s.clone());
    s
}

pub fn confidence() -> f32 {
    match CONF_BITS.load(Relaxed) {
        0 => current().confidence,
        b => f32::from_bits(b),
    }
}

pub fn overlap() -> f32 {
    match OVERLAP_BITS.load(Relaxed) {
        0 => current().overlap,
        b => f32::from_bits(b),
    }
}

/// `MODEL` in the environment wins (scripts, benchmarks); otherwise the saved choice.
pub fn model_from_env() -> Option<String> {
    std::env::var("MODEL").ok().filter(|m| !m.trim().is_empty())
}

/// The model the running engine actually loaded (fixed at start; the saved choice may differ until the next reload).
static LOADED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
pub fn loaded_model() -> String {
    LOADED.get_or_init(active_model).clone()
}

pub fn active_model() -> String {
    model_from_env().unwrap_or_else(|| current().model)
}

/// `models/yolo26m_736x1280.onnx` -> (height 736, width 1280). The real size is read from the file when it is loaded;
/// the name is only used to list models and to pick a sensible batch size before loading.
pub fn parse_res(file: &str) -> Option<(usize, usize)> {
    let stem = file.rsplit('/').next()?.trim_end_matches(".onnx");
    let tail = stem.split('_').find(|p| p.contains('x') && p.split('x').all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())))?;
    let (h, w) = tail.split_once('x')?;
    Some((h.parse().ok()?, w.parse().ok()?))
}

/// Cameras per batch: more pixels per frame means fewer fit in GPU memory and in a real-time budget.
pub fn auto_batch(width: usize, trt: bool) -> usize {
    let base = if width <= 960 { 4 } else if width <= 1280 { 3 } else { 2 };
    if trt { base } else { base * 2 }
}

/// What the engine will use at start: `MAX_BATCH` env, else the saved number, else automatic.
pub fn effective_batch(model: &str, trt: bool) -> usize {
    if let Some(n) = std::env::var("MAX_BATCH").ok().and_then(|v| v.parse::<usize>().ok()).filter(|n| *n > 0) {
        return n;
    }
    match current().max_batch {
        0 => auto_batch(parse_res(model).map_or(960, |(_, w)| w), trt),
        n => n as usize,
    }
}
