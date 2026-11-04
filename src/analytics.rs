//! Everything Chira remembers lives in one SQLite file (`data/chira.db`, WAL):
//!   events     line crossings, zone enter/exit, raw alarm firings   (the Analytics tab aggregates these)
//!   occ        zone occupancy samples every 2 s
//!   incidents  alarms as a person wants to see them: repeated firings of one rule on one camera are
//!              GROUPED into a single incident (with a count and a snapshot), which can be acknowledged
//!   snoozes    rules a person muted for a while (they come back by themselves)
//!   sources    the cameras that were added, so a restart brings them back
//! Hot-path writes (events, occupancy, incidents) go through a writer thread that batches them in
//! transactions, so inference threads never wait for the disk.

use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{channel, Receiver, Sender};
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

/// One alarm as shown to people. Firings of the same rule on the same camera within `GROUP_MS` of each other
/// are merged into one incident.
#[derive(Clone, Serialize)]
pub struct Incident {
    pub id: u64,
    pub rule_id: String,
    pub rule: String,
    pub cam: String,
    pub kind: String,
    pub target: String,
    /// "info" | "warning" | "critical"
    pub severity: String,
    pub cls: String,
    pub msg: String,
    pub first_t: u64,
    pub last_t: u64,
    pub count: u32,
    /// 0 = nobody has acknowledged it yet
    pub acked_t: u64,
    /// the object that triggered it, in 960x540 frame pixels (for the snapshot highlight)
    pub bbox: Option<[f32; 4]>,
    pub snap: bool,
}

pub struct Fire {
    pub cam: String,
    pub rule_id: String,
    pub rule: String,
    pub kind: String,
    pub severity: String,
    pub target: String,
    pub cls: String,
    pub msg: String,
    pub bbox: Option<[f32; 4]>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    /// configuration key (lines, zones, alarms are stored per key)
    pub key: String,
    /// "demo" | "url"
    pub kind: String,
    pub setup: u32,
    pub fps: u32,
    #[serde(default)]
    pub url: String,
}

/// quiet time after which a new firing starts a new incident instead of joining the open one
pub const GROUP_MS: u64 = 120_000;
const KEEP_EVENTS_DAYS: u64 = 30;
const KEEP_INCIDENTS_DAYS: u64 = 120;

enum Cmd {
    Event(Event),
    Occ(Occ),
    Incident(Incident),
}

pub struct Hub {
    tx: Sender<Cmd>,
    db: Mutex<Connection>,
    open: Mutex<HashMap<(String, String), Incident>>,
    snoozes: Mutex<HashMap<(String, String), u64>>,
    next_id: AtomicU64,
    pub alerts: broadcast::Sender<String>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn snap_dir() -> String {
    format!("{}/snapshots", crate::config::data_dir())
}

fn open_db(path: &str) -> rusqlite::Result<Connection> {
    let c = Connection::open(path)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    Ok(c)
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (t INTEGER NOT NULL, cam TEXT NOT NULL, setup INTEGER, kind TEXT NOT NULL, target TEXT, cls TEXT, dir TEXT, dwell REAL, rule TEXT, msg TEXT, track INTEGER);
CREATE INDEX IF NOT EXISTS events_t ON events(t);
CREATE TABLE IF NOT EXISTS occ (t INTEGER NOT NULL, cam TEXT NOT NULL, zone TEXT NOT NULL, n INTEGER, cap INTEGER);
CREATE INDEX IF NOT EXISTS occ_t ON occ(t);
CREATE TABLE IF NOT EXISTS incidents (id INTEGER PRIMARY KEY, rule_id TEXT, rule TEXT, cam TEXT, kind TEXT, target TEXT, severity TEXT, cls TEXT, msg TEXT,
  first_t INTEGER, last_t INTEGER, count INTEGER, acked_t INTEGER DEFAULT 0, bbox TEXT, snap INTEGER DEFAULT 0);
CREATE INDEX IF NOT EXISTS incidents_t ON incidents(last_t);
CREATE TABLE IF NOT EXISTS snoozes (cam TEXT, rule_id TEXT, until INTEGER, PRIMARY KEY (cam, rule_id));
CREATE TABLE IF NOT EXISTS sources (name TEXT PRIMARY KEY, key TEXT, kind TEXT, setup INTEGER, fps INTEGER, url TEXT);
";

fn write_batch(c: &mut Connection, batch: &mut Vec<Cmd>) {
    if batch.is_empty() {
        return;
    }
    let Ok(tx) = c.transaction() else { return };
    for cmd in batch.drain(..) {
        let _ = match cmd {
            Cmd::Event(e) => tx.execute(
                "INSERT INTO events VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                params![e.t as i64, e.cam, e.setup, e.kind, e.target, e.cls, e.dir, e.dwell, e.rule, e.msg, e.track],
            ),
            Cmd::Occ(o) => tx.execute("INSERT INTO occ VALUES (?,?,?,?,?)", params![o.t as i64, o.cam, o.zone, o.n, o.cap]),
            Cmd::Incident(i) => tx.execute(
                "INSERT OR REPLACE INTO incidents VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params![i.id as i64, i.rule_id, i.rule, i.cam, i.kind, i.target, i.severity, i.cls, i.msg, i.first_t as i64, i.last_t as i64, i.count, i.acked_t as i64,
                        i.bbox.map(|b| serde_json::to_string(&b).unwrap_or_default()), i.snap as i32],
            ),
        };
    }
    let _ = tx.commit();
}

fn writer(mut c: Connection, rx: Receiver<Cmd>) {
    let mut batch = Vec::new();
    let mut last_prune = std::time::Instant::now();
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(400)) {
            Ok(cmd) => {
                batch.push(cmd);
                while batch.len() < 500 {
                    match rx.try_recv() {
                        Ok(c) => batch.push(c),
                        Err(_) => break,
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
        write_batch(&mut c, &mut batch);
        if last_prune.elapsed().as_secs() > 3600 {
            prune(&c);
            last_prune = std::time::Instant::now();
        }
    }
}

fn prune(c: &Connection) {
    let day = 86_400_000u64;
    let _ = c.execute("DELETE FROM events WHERE t < ?", params![now_ms().saturating_sub(KEEP_EVENTS_DAYS * day) as i64]);
    let _ = c.execute("DELETE FROM occ WHERE t < ?", params![now_ms().saturating_sub(KEEP_EVENTS_DAYS * day) as i64]);
    let cutoff = now_ms().saturating_sub(KEEP_INCIDENTS_DAYS * day) as i64;
    if let Ok(mut st) = c.prepare("SELECT id FROM incidents WHERE last_t < ?") {
        if let Ok(rows) = st.query_map(params![cutoff], |r| r.get::<_, i64>(0)) {
            for id in rows.flatten() {
                let _ = std::fs::remove_file(format!("{}/{id}.jpg", snap_dir()));
            }
        }
    }
    let _ = c.execute("DELETE FROM incidents WHERE last_t < ?", params![cutoff]);
    let _ = c.execute("DELETE FROM snoozes WHERE until < ?", params![now_ms() as i64]);
}

/// One-time import of the previous JSON-lines store.
fn migrate_jsonl(c: &mut Connection, dir: &str) {
    let path = format!("{dir}/events.jsonl");
    let Ok(s) = std::fs::read_to_string(&path) else { return };
    let mut batch: Vec<Cmd> = s.lines().filter_map(|l| serde_json::from_str::<Event>(l).ok()).map(Cmd::Event).collect();
    let n = batch.len();
    write_batch(c, &mut batch);
    let _ = std::fs::rename(&path, format!("{path}.migrated"));
    println!("imported {n} events from events.jsonl into chira.db");
}

impl Hub {
    pub fn new() -> Self {
        let dir = crate::config::data_dir();
        let _ = std::fs::create_dir_all(snap_dir());
        let path = format!("{dir}/chira.db");
        let mut db = open_db(&path).expect("open sqlite");
        db.execute_batch(SCHEMA).expect("create schema");
        migrate_jsonl(&mut db, &dir);
        prune(&db);
        let next: i64 = db.query_row("SELECT COALESCE(MAX(id),0) FROM incidents", [], |r| r.get(0)).unwrap_or(0);
        let mut snoozes = HashMap::new();
        if let Ok(mut st) = db.prepare("SELECT cam, rule_id, until FROM snoozes WHERE until > ?") {
            if let Ok(rows) = st.query_map(params![now_ms() as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))) {
                for (c, r, u) in rows.flatten() {
                    snoozes.insert((c, r), u as u64);
                }
            }
        }
        let (tx, rx) = channel();
        let wc = open_db(&path).expect("open sqlite (writer)");
        std::thread::Builder::new().name("db-writer".into()).spawn(move || writer(wc, rx)).unwrap();
        let (alerts, _) = broadcast::channel(64);
        Self { tx, db: Mutex::new(db), open: Mutex::new(HashMap::new()), snoozes: Mutex::new(snoozes), next_id: AtomicU64::new(next as u64), alerts }
    }

    pub fn push(&self, e: Event) {
        let _ = self.tx.send(Cmd::Event(e));
    }

    pub fn push_occ(&self, o: Occ) {
        let _ = self.tx.send(Cmd::Occ(o));
    }

    pub fn is_snoozed(&self, cam: &str, rule: &str) -> bool {
        self.snoozes.lock().get(&(cam.to_string(), rule.to_string())).map_or(false, |&u| u > now_ms())
    }

    /// A rule fired. Joins the open incident of that rule+camera, or opens a new one (with a snapshot) and tells the UIs.
    pub fn fire(&self, f: Fire, snapshot: impl FnOnce() -> Option<Vec<u8>>) {
        if self.is_snoozed(&f.cam, &f.rule_id) {
            return;
        }
        let now = now_ms();
        let key = (f.cam.clone(), f.rule_id.clone());
        let mut open = self.open.lock();
        open.retain(|_, i| now.saturating_sub(i.last_t) < GROUP_MS * 4);
        if let Some(inc) = open.get_mut(&key) {
            if now.saturating_sub(inc.last_t) < GROUP_MS {
                inc.last_t = now;
                inc.count += 1;
                inc.msg = f.msg;
                let snap = inc.clone();
                drop(open);
                let _ = self.tx.send(Cmd::Incident(snap.clone()));
                self.broadcast("incident", false, &snap);
                return;
            }
        }
        let id = self.next_id.fetch_add(1, Relaxed) + 1;
        let snap_ok = snapshot().map(|jpg| std::fs::write(format!("{}/{id}.jpg", snap_dir()), jpg).is_ok()).unwrap_or(false);
        let inc = Incident {
            id, rule_id: f.rule_id, rule: f.rule, cam: f.cam, kind: f.kind, target: f.target, severity: f.severity, cls: f.cls, msg: f.msg,
            first_t: now, last_t: now, count: 1, acked_t: 0, bbox: f.bbox, snap: snap_ok,
        };
        open.insert(key, inc.clone());
        drop(open);
        let _ = self.tx.send(Cmd::Incident(inc.clone()));
        self.broadcast("incident", true, &inc);
    }

    fn broadcast(&self, ty: &str, new: bool, inc: &Incident) {
        let _ = self.alerts.send(serde_json::json!({"type": ty, "new": new, "incident": inc}).to_string());
    }

    /// Acknowledge one incident (`id`) or every unacknowledged one (`None`, optionally limited to one camera).
    pub fn ack(&self, id: Option<u64>, cam: Option<&str>) -> Vec<u64> {
        let now = now_ms();
        let db = self.db.lock();
        let sql = "SELECT id FROM incidents WHERE acked_t = 0".to_string() + if id.is_some() { " AND id = ?1" } else if cam.is_some() { " AND cam = ?1" } else { "" };
        let mut ids: Vec<u64> = Vec::new();
        if let Ok(mut st) = db.prepare(&sql) {
            let rows: Vec<i64> = match (id, cam) {
                (Some(i), _) => st.query_map(params![i as i64], |r| r.get(0)).map(|r| r.flatten().collect()).unwrap_or_default(),
                (None, Some(c)) => st.query_map(params![c], |r| r.get(0)).map(|r| r.flatten().collect()).unwrap_or_default(),
                _ => st.query_map([], |r| r.get(0)).map(|r| r.flatten().collect()).unwrap_or_default(),
            };
            ids = rows.into_iter().map(|i| i as u64).collect();
        }
        for i in &ids {
            let _ = db.execute("UPDATE incidents SET acked_t = ? WHERE id = ?", params![now as i64, *i as i64]);
        }
        drop(db);
        for inc in self.open.lock().values_mut() {
            if ids.contains(&inc.id) || (id.is_none() && inc.acked_t == 0 && cam.map_or(true, |c| c == inc.cam)) {
                inc.acked_t = now;
            }
        }
        let _ = self.alerts.send(serde_json::json!({"type": "ack", "ids": ids, "t": now}).to_string());
        ids
    }

    pub fn snooze(&self, cam: &str, rule: &str, minutes: u64) {
        let until = now_ms() + minutes * 60_000;
        self.snoozes.lock().insert((cam.to_string(), rule.to_string()), until);
        let _ = self.db.lock().execute("INSERT OR REPLACE INTO snoozes VALUES (?,?,?)", params![cam, rule, until as i64]);
        let _ = self.alerts.send(serde_json::json!({"type": "snooze"}).to_string());
    }

    pub fn unsnooze(&self, cam: &str, rule: &str) {
        self.snoozes.lock().remove(&(cam.to_string(), rule.to_string()));
        let _ = self.db.lock().execute("DELETE FROM snoozes WHERE cam = ? AND rule_id = ?", params![cam, rule]);
        let _ = self.alerts.send(serde_json::json!({"type": "snooze"}).to_string());
    }

    pub fn snoozes(&self) -> Vec<serde_json::Value> {
        let now = now_ms();
        self.snoozes.lock().iter().filter(|(_, &u)| u > now).map(|((c, r), &u)| serde_json::json!({"cam": c, "rule_id": r, "until": u})).collect()
    }

    pub fn incidents(&self, limit: usize) -> Vec<Incident> {
        let db = self.db.lock();
        let Ok(mut st) = db.prepare(
            "SELECT id, rule_id, rule, cam, kind, target, severity, cls, msg, first_t, last_t, count, acked_t, bbox, snap FROM incidents ORDER BY last_t DESC LIMIT ?",
        ) else {
            return vec![];
        };
        let rows = st.query_map(params![limit as i64], |r| {
            let bbox: Option<String> = r.get(13)?;
            Ok(Incident {
                id: r.get::<_, i64>(0)? as u64,
                rule_id: r.get(1)?, rule: r.get(2)?, cam: r.get(3)?, kind: r.get(4)?, target: r.get(5)?, severity: r.get(6)?, cls: r.get(7)?, msg: r.get(8)?,
                first_t: r.get::<_, i64>(9)? as u64, last_t: r.get::<_, i64>(10)? as u64, count: r.get(11)?, acked_t: r.get::<_, i64>(12)? as u64,
                bbox: bbox.and_then(|s| serde_json::from_str(&s).ok()), snap: r.get::<_, i32>(14)? != 0,
            })
        });
        rows.map(|r| r.flatten().collect()).unwrap_or_default()
    }

    pub fn query(&self, since: u64, limit: usize) -> (Vec<Event>, Vec<Occ>) {
        let db = self.db.lock();
        let mut ev: Vec<Event> = Vec::new();
        if let Ok(mut st) = db.prepare("SELECT t, cam, setup, kind, target, cls, dir, dwell, rule, msg, track FROM events WHERE t >= ? ORDER BY t DESC LIMIT ?") {
            if let Ok(rows) = st.query_map(params![since as i64, limit as i64], |r| {
                Ok(Event {
                    t: r.get::<_, i64>(0)? as u64, cam: r.get(1)?, setup: r.get::<_, Option<u32>>(2)?.unwrap_or(0), kind: r.get(3)?,
                    target: r.get::<_, Option<String>>(4)?.unwrap_or_default(), cls: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    dir: r.get::<_, Option<String>>(6)?.unwrap_or_default(), dwell: r.get::<_, Option<f32>>(7)?.unwrap_or(0.0),
                    rule: r.get::<_, Option<String>>(8)?.unwrap_or_default(), msg: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                    track: r.get::<_, Option<u32>>(10)?.unwrap_or(0),
                })
            }) {
                ev = rows.flatten().collect();
            }
        }
        ev.reverse();
        let mut occ: Vec<Occ> = Vec::new();
        if let Ok(mut st) = db.prepare("SELECT t, cam, zone, n, cap FROM occ WHERE t >= ? ORDER BY t") {
            if let Ok(rows) = st.query_map(params![since as i64], |r| {
                Ok(Occ { t: r.get::<_, i64>(0)? as u64, cam: r.get(1)?, zone: r.get(2)?, n: r.get::<_, Option<u32>>(3)?.unwrap_or(0), cap: r.get::<_, Option<u32>>(4)?.unwrap_or(0) })
            }) {
                occ = rows.flatten().collect();
            }
        }
        (ev, occ)
    }

    // ----- cameras that were added (restored on restart) -----
    pub fn save_source(&self, s: &Source) {
        let _ = self.db.lock().execute("INSERT OR REPLACE INTO sources VALUES (?,?,?,?,?,?)", params![s.name, s.key, s.kind, s.setup, s.fps, s.url]);
    }
    pub fn delete_source(&self, name: &str) {
        let _ = self.db.lock().execute("DELETE FROM sources WHERE name = ?", params![name]);
    }
    pub fn sources(&self) -> Vec<Source> {
        let db = self.db.lock();
        let Ok(mut st) = db.prepare("SELECT name, key, kind, setup, fps, url FROM sources ORDER BY rowid") else { return vec![] };
        let rows = st.query_map([], |r| {
            Ok(Source { name: r.get(0)?, key: r.get(1)?, kind: r.get(2)?, setup: r.get::<_, Option<u32>>(3)?.unwrap_or(0), fps: r.get::<_, Option<u32>>(4)?.unwrap_or(30), url: r.get::<_, Option<String>>(5)?.unwrap_or_default() })
        });
        rows.map(|r| r.flatten().collect()).unwrap_or_default()
    }
}
