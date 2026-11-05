//! Per-camera configuration: which classes to track, counting lines, zones (with capacity) and alarm rules.
//! Normalised (0..1) coordinates so it is independent of resolution. Saved per preset in `data/`.

use serde::{Deserialize, Serialize};

/// Detectable classes (COCO ids), grouped for the UI: people, road vehicles, other vehicles, animals, carried items.
/// The other ~65 COCO classes (furniture, food, kitchenware...) are deliberately left out: noise for a camera app.
pub const CLASSES: [(&str, u8); 15] = [
    ("person", 0), ("car", 2), ("truck", 7), ("bus", 5), ("motorcycle", 3), ("bicycle", 1), ("train", 6), ("boat", 8),
    ("dog", 16), ("cat", 15), ("bird", 14), ("horse", 17), ("backpack", 24), ("handbag", 26), ("suitcase", 28),
];
/// What a camera tracks until the person chooses otherwise (the traffic demos).
pub const DEFAULT_CLASSES: [&str; 4] = ["car", "truck", "bus", "motorcycle"];

pub fn class_name(c: u8) -> &'static str {
    CLASSES.iter().find(|(_, id)| *id == c).map(|(n, _)| *n).unwrap_or("other")
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct LineCfg {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ZoneCfg {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub points: Vec<[f32; 2]>,
    /// "max M vehicles": occupancy % = vehicles inside / capacity
    #[serde(default = "default_capacity")]
    pub capacity: u32,
    /// empty = every class the camera tracks
    #[serde(default)]
    pub classes: Vec<String>,
}
fn default_capacity() -> u32 {
    10
}

/// kind: "enter" | "exit" | "dwell" (stays >= seconds) | "occupancy" (>= percent of capacity) | "cross" (line)
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct AlarmRule {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub kind: String,
    /// zone id (enter/exit/dwell/occupancy) or line id (cross)
    pub target: String,
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub seconds: f32,
    #[serde(default)]
    pub percent: f32,
    /// cross rules: "any" | "fwd" | "rev"
    #[serde(default)]
    pub direction: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// "info" (logged only) | "warning" (badge + camera marker) | "critical" (also a notification and sound)
    #[serde(default = "warning")]
    pub severity: String,
}
fn warning() -> String {
    "warning".into()
}
fn yes() -> bool {
    true
}

#[derive(Clone, Serialize, Deserialize, Default, Debug)]
pub struct CameraConfig {
    pub classes: Vec<String>,
    #[serde(default)]
    pub lines: Vec<LineCfg>,
    #[serde(default)]
    pub zones: Vec<ZoneCfg>,
    #[serde(default)]
    pub alarms: Vec<AlarmRule>,
}

fn short_id(prefix: &str, i: usize) -> String {
    format!("{prefix}{}", i + 1)
}

impl CameraConfig {
    /// Clamp / repair user input so the engines never see garbage.
    pub fn sanitize(&mut self) {
        self.classes.retain(|c| CLASSES.iter().any(|(n, _)| n == c));
        self.classes.dedup();
        if self.classes.is_empty() {
            self.classes = DEFAULT_CLASSES.iter().map(|n| n.to_string()).collect();
        }
        let cl = |v: f32| v.clamp(0.0, 1.0);
        for (i, l) in self.lines.iter_mut().enumerate() {
            if l.id.is_empty() {
                l.id = short_id("l", i);
            }
            l.name = l.name.trim().chars().take(24).collect();
            if l.name.is_empty() {
                l.name = format!("Line {}", i + 1);
            }
            (l.x1, l.y1, l.x2, l.y2) = (cl(l.x1), cl(l.y1), cl(l.x2), cl(l.y2));
        }
        self.lines.retain(|l| (l.x1 - l.x2).abs() + (l.y1 - l.y2).abs() > 0.01);
        for (i, z) in self.zones.iter_mut().enumerate() {
            if z.id.is_empty() {
                z.id = short_id("z", i);
            }
            z.name = z.name.trim().chars().take(24).collect();
            if z.name.is_empty() {
                z.name = format!("Zone {}", i + 1);
            }
            z.capacity = z.capacity.clamp(1, 999);
            for p in z.points.iter_mut() {
                *p = [cl(p[0]), cl(p[1])];
            }
            z.classes.retain(|c| CLASSES.iter().any(|(n, _)| n == c));
        }
        self.zones.retain(|z| z.points.len() >= 3);
        let (zones, lines): (Vec<String>, Vec<String>) = (self.zones.iter().map(|z| z.id.clone()).collect(), self.lines.iter().map(|l| l.id.clone()).collect());
        for (i, a) in self.alarms.iter_mut().enumerate() {
            if a.id.is_empty() {
                a.id = short_id("a", i);
            }
            if a.name.trim().is_empty() {
                a.name = format!("Alarm {}", i + 1);
            }
            if !["info", "warning", "critical"].contains(&a.severity.as_str()) {
                a.severity = "warning".into();
            }
            a.seconds = a.seconds.clamp(0.0, 3600.0);
            a.percent = a.percent.clamp(1.0, 200.0);
            a.classes.retain(|c| CLASSES.iter().any(|(n, _)| n == c));
        }
        self.alarms.retain(|a| match a.kind.as_str() {
            "cross" => lines.contains(&a.target),
            "enter" | "exit" | "dwell" | "occupancy" => zones.contains(&a.target),
            _ => false,
        });
    }

    pub fn allowed_class_ids(&self) -> [bool; 256] {
        let mut ok = [false; 256];
        for (n, id) in CLASSES {
            if self.classes.iter().any(|c| c == n) {
                ok[id as usize] = true;
            }
        }
        ok
    }
}

pub fn data_dir() -> String {
    std::env::var("DATA_DIR").unwrap_or_else(|_| "data".into())
}

/// `key`: "setup1" / "setup2" for the demo cameras, "cam-<slug>" for real ones (so `config_setup1.json` files keep working)
fn cfg_path(key: &str) -> String {
    format!("{}/config_{key}.json", data_dir())
}

pub fn load(key: &str) -> Option<CameraConfig> {
    let s = std::fs::read_to_string(cfg_path(key)).ok()?;
    let mut c: CameraConfig = serde_json::from_str(&s).ok()?;
    c.sanitize();
    Some(c)
}

pub fn save(key: &str, c: &CameraConfig) {
    let _ = std::fs::create_dir_all(data_dir());
    if let Ok(s) = serde_json::to_string_pretty(c) {
        let _ = std::fs::write(cfg_path(key), s);
    }
}
