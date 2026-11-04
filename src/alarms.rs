//! Alarm rules evaluated every frame against zone events, line crossings and zone state.
//!   enter     a matching vehicle enters the zone
//!   exit      a matching vehicle leaves the zone
//!   dwell     a matching vehicle has stayed in the zone for >= `seconds`  (fires once per visit)
//!   occupancy the zone is >= `percent` full (fires on the way up, re-arms 10 points below)
//!   cross     a matching vehicle crosses a counting line (optionally only one direction)

use crate::config::{class_name, AlarmRule, CameraConfig};
use crate::counter::CrossEv;
use crate::zones::{ZoneEngine, ZoneEv, ZoneEvKind};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

pub struct Fired {
    pub rule_id: String,
    pub rule_name: String,
    pub kind: String,
    pub target_name: String,
    pub target_id: String,
    pub cls: String,
    pub track: u32,
    pub msg: String,
}

#[derive(Default)]
pub struct AlarmEngine {
    rules: Vec<AlarmRule>,
    fired_dwell: HashSet<(String, u32)>,
    armed: HashMap<String, bool>,
    last_fire: HashMap<String, Instant>,
}

fn matches(rule: &AlarmRule, cls: u8) -> bool {
    rule.classes.is_empty() || rule.classes.iter().any(|c| c == class_name(cls))
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

impl AlarmEngine {
    pub fn reconfigure(&mut self, cfg: &CameraConfig) {
        self.rules = cfg.alarms.iter().filter(|a| a.enabled).cloned().collect();
        let ids: HashSet<&String> = self.rules.iter().map(|r| &r.id).collect();
        self.armed.retain(|k, _| ids.contains(k));
        self.fired_dwell.retain(|(k, _)| ids.contains(k));
    }

    pub fn eval(&mut self, zone_ev: &[ZoneEv], cross_ev: &[CrossEv], zones: &ZoneEngine, now: Instant) -> Vec<Fired> {
        let mut out = Vec::new();
        for r in &self.rules {
            match r.kind.as_str() {
                "enter" | "exit" => {
                    for e in zone_ev.iter().filter(|e| e.zone_id == r.target && matches(r, e.cls)) {
                        let is_enter = matches!(e.kind, ZoneEvKind::Enter);
                        if is_enter != (r.kind == "enter") {
                            continue;
                        }
                        let msg = if is_enter { format!("{} entered {}", cap(class_name(e.cls)), e.zone_name) } else { format!("{} left {}", cap(class_name(e.cls)), e.zone_name) };
                        out.push(Fired { rule_id: r.id.clone(), rule_name: r.name.clone(), kind: r.kind.clone(), target_name: e.zone_name.clone(), target_id: e.zone_id.clone(), cls: class_name(e.cls).into(), track: e.track, msg });
                    }
                }
                "dwell" => {
                    for (zid, track, cls, dwell) in zones.dwell(now) {
                        if zid == r.target && matches(r, cls) && dwell >= r.seconds && self.fired_dwell.insert((r.id.clone(), track)) {
                            let name = zones.occupancy().find(|z| z.0 == zid).map(|z| z.1.to_string()).unwrap_or_default();
                            out.push(Fired {
                                rule_id: r.id.clone(),
                                rule_name: r.name.clone(),
                                kind: "dwell".into(),
                                target_name: name.clone(),
                                target_id: zid.to_string(),
                                cls: class_name(cls).into(),
                                track,
                                msg: format!("{} has stayed in {} for {:.0} s", cap(class_name(cls)), name, dwell),
                            });
                        }
                    }
                }
                "occupancy" => {
                    if let Some((zid, name, n, capy)) = zones.occupancy().find(|z| z.0 == r.target) {
                        let pct = 100.0 * n as f32 / capy.max(1) as f32;
                        let armed = self.armed.entry(r.id.clone()).or_insert(true);
                        if *armed && pct >= r.percent {
                            *armed = false;
                            out.push(Fired {
                                rule_id: r.id.clone(),
                                rule_name: r.name.clone(),
                                kind: "occupancy".into(),
                                target_name: name.to_string(),
                                target_id: zid.to_string(),
                                cls: String::new(),
                                track: 0,
                                msg: format!("{} is {:.0}% full ({}/{})", name, pct, n, capy),
                            });
                        } else if !*armed && pct < r.percent - 10.0 {
                            *armed = true;
                        }
                    }
                }
                "cross" => {
                    for e in cross_ev.iter().filter(|e| e.line_id == r.target && matches(r, e.cls)) {
                        if (r.direction == "fwd" && !e.fwd) || (r.direction == "rev" && e.fwd) {
                            continue;
                        }
                        out.push(Fired { rule_id: r.id.clone(), rule_name: r.name.clone(), kind: "cross".into(), target_name: e.line_name.clone(), target_id: e.line_id.clone(), cls: class_name(e.cls).into(), track: e.track, msg: format!("{} crossed {}", cap(class_name(e.cls)), e.line_name) });
                    }
                }
                _ => {}
            }
        }
        // forget dwell state of visits that ended
        for e in zone_ev {
            if let ZoneEvKind::Exit { .. } = e.kind {
                self.fired_dwell.retain(|(_, t)| *t != e.track);
            }
        }
        for f in &out {
            self.last_fire.insert(f.rule_id.clone(), now);
        }
        out
    }
}
