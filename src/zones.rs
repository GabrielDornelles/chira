//! Zones: polygons with a capacity. Tracks the vehicles inside each one (anchor = bottom-centre of the box,
//! i.e. where the vehicle touches the road), their dwell times, occupancy %, and enter / exit events.

use crate::config::ZoneCfg;
use crate::tracker::TrackOut;
use serde::Serialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Serialize)]
pub struct ZoneSnap {
    pub id: String,
    pub name: String,
    pub points: Vec<[f32; 2]>,
    pub cap: u32,
    pub n: u32,
}

pub struct Visit {
    pub entered: Instant,
    pub last_seen: Instant,
    pub cls: u8,
}

pub enum ZoneEvKind {
    Enter,
    Exit { dwell_s: f32 },
}
pub struct ZoneEv {
    pub zone_id: String,
    pub zone_name: String,
    pub track: u32,
    pub cls: u8,
    pub kind: ZoneEvKind,
}

struct Zone {
    cfg: ZoneCfg,
    px: Vec<(f32, f32)>,
    visits: HashMap<u32, Visit>,
    n: u32,
}

pub struct ZoneEngine {
    zones: Vec<Zone>,
    w: f32,
    h: f32,
    /// a track not seen for this long has left (tolerates a few missed detections)
    grace: Duration,
}

fn inside(poly: &[(f32, f32)], p: (f32, f32)) -> bool {
    let mut c = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            c = !c;
        }
        j = i;
    }
    c
}

impl ZoneEngine {
    pub fn new(zones: &[ZoneCfg], w: f32, h: f32) -> Self {
        let mut e = Self { zones: vec![], w, h, grace: Duration::from_millis(800) };
        e.reconfigure(zones);
        e
    }

    pub fn reconfigure(&mut self, zones: &[ZoneCfg]) {
        let mut new = Vec::new();
        for z in zones {
            let visits = self.zones.iter_mut().find(|o| o.cfg.id == z.id).map(|o| std::mem::take(&mut o.visits)).unwrap_or_default();
            let px = z.points.iter().map(|p| (p[0] * self.w, p[1] * self.h)).collect();
            new.push(Zone { cfg: z.clone(), px, visits, n: 0 });
        }
        self.zones = new;
    }

    pub fn snapshot(&self) -> Vec<ZoneSnap> {
        self.zones
            .iter()
            .map(|z| ZoneSnap { id: z.cfg.id.clone(), name: z.cfg.name.clone(), points: z.cfg.points.clone(), cap: z.cfg.capacity, n: z.n })
            .collect()
    }

    /// (zone id, zone name, occupancy, capacity)
    pub fn occupancy(&self) -> impl Iterator<Item = (&str, &str, u32, u32)> {
        self.zones.iter().map(|z| (z.cfg.id.as_str(), z.cfg.name.as_str(), z.n, z.cfg.capacity))
    }

    /// (zone id, track, class, dwell seconds) for everything currently inside
    pub fn dwell(&self, now: Instant) -> Vec<(&str, u32, u8, f32)> {
        let mut v = Vec::new();
        for z in &self.zones {
            for (t, vis) in &z.visits {
                if now.duration_since(vis.last_seen) < self.grace {
                    v.push((z.cfg.id.as_str(), *t, vis.cls, vis.last_seen.duration_since(vis.entered).as_secs_f32()));
                }
            }
        }
        v
    }

    pub fn update(&mut self, tracks: &[TrackOut], now: Instant) -> Vec<ZoneEv> {
        let mut ev = Vec::new();
        for z in &mut self.zones {
            let mut n = 0;
            for t in tracks {
                if !z.cfg.classes.is_empty() && !z.cfg.classes.iter().any(|c| c == crate::config::class_name(t.cls)) {
                    continue;
                }
                let anchor = ((t.bbox[0] + t.bbox[2]) / 2.0, t.bbox[3]);
                if inside(&z.px, anchor) {
                    n += 1;
                    match z.visits.get_mut(&t.id) {
                        Some(v) => v.last_seen = now,
                        None => {
                            z.visits.insert(t.id, Visit { entered: now, last_seen: now, cls: t.cls });
                            ev.push(ZoneEv { zone_id: z.cfg.id.clone(), zone_name: z.cfg.name.clone(), track: t.id, cls: t.cls, kind: ZoneEvKind::Enter });
                        }
                    }
                }
            }
            z.n = n;
            let grace = self.grace;
            let gone: Vec<u32> = z.visits.iter().filter(|(_, v)| now.duration_since(v.last_seen) > grace).map(|(t, _)| *t).collect();
            for t in gone {
                if let Some(v) = z.visits.remove(&t) {
                    ev.push(ZoneEv {
                        zone_id: z.cfg.id.clone(),
                        zone_name: z.cfg.name.clone(),
                        track: t,
                        cls: v.cls,
                        kind: ZoneEvKind::Exit { dwell_s: v.last_seen.duration_since(v.entered).as_secs_f32() },
                    });
                }
            }
        }
        ev
    }
}
