//! Event store: line crossings, zone enter/exit, alarms (persisted as JSON lines) and zone occupancy samples
//! (memory only). The analytics tab aggregates these client-side; alarms are also pushed live to the UI.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::Write;
use tokio::sync::broadcast;

#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub t: u64,
    pub cam: String,
    pub setup: u32,
    /// "cross" | "enter" | "exit" | "alarm"
    pub kind: String,
    pub target: String,
    #[serde(default)]
    pub cls: String,
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub dwell: f32,
    #[serde(default)]
    pub rule: String,
    #[serde(default)]
    pub msg: String,
    #[serde(default)]
    pub track: u32,
}

#[derive(Clone, Serialize)]
pub struct Occ {
    pub t: u64,
    pub cam: String,
    pub zone: String,
    pub n: u32,
    pub cap: u32,
}

pub struct Hub {
    events: Mutex<VecDeque<Event>>,
    occ: Mutex<VecDeque<Occ>>,
    file: Mutex<Option<std::fs::File>>,
    pub alerts: broadcast::Sender<String>,
}

const MAX_EVENTS: usize = 60_000;
const MAX_OCC: usize = 30_000;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Hub {
    pub fn new() -> Self {
        let dir = crate::config::data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = format!("{dir}/events.jsonl");
        let mut events = VecDeque::new();
        if let Ok(s) = std::fs::read_to_string(&path) {
            for l in s.lines() {
                if let Ok(e) = serde_json::from_str::<Event>(l) {
                    events.push_back(e);
                    if events.len() > MAX_EVENTS {
                        events.pop_front();
                    }
                }
            }
        }
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
        let (alerts, _) = broadcast::channel(64);
        Self { events: Mutex::new(events), occ: Mutex::new(VecDeque::new()), file: Mutex::new(file), alerts }
    }

    pub fn push(&self, e: Event) {
        if let Some(f) = self.file.lock().as_mut() {
            if let Ok(s) = serde_json::to_string(&e) {
                let _ = writeln!(f, "{s}");
            }
        }
        let mut q = self.events.lock();
        q.push_back(e);
        if q.len() > MAX_EVENTS {
            q.pop_front();
        }
    }

    pub fn push_occ(&self, o: Occ) {
        let mut q = self.occ.lock();
        q.push_back(o);
        if q.len() > MAX_OCC {
            q.pop_front();
        }
    }

    pub fn alert(&self, e: &Event) {
        if let Ok(s) = serde_json::to_string(e) {
            let _ = self.alerts.send(s);
        }
    }

    pub fn query(&self, since: u64, limit: usize) -> (Vec<Event>, Vec<Occ>) {
        let ev: Vec<Event> = self.events.lock().iter().rev().filter(|e| e.t >= since).take(limit).cloned().collect();
        let occ: Vec<Occ> = self.occ.lock().iter().filter(|o| o.t >= since).cloned().collect();
        (ev.into_iter().rev().collect(), occ)
    }
}
