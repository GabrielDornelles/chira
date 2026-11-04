//! Line crossing counter. Lines are in normalised (0..1) frame coordinates.

use crate::config::{class_name, LineCfg};
use crate::tracker::TrackOut;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Serialize, Default)]
pub struct LineCounts {
    pub id: String,
    pub name: String,
    /// crossings towards the right-hand side of the A->B direction (screen coords: "down" for a left-to-right line)
    pub fwd: u32,
    pub rev: u32,
    /// totals per class name (car, truck, ...)
    pub by_class: HashMap<String, u32>,
}

#[derive(Clone)]
pub struct CrossEv {
    pub line_id: String,
    pub line_name: String,
    pub track: u32,
    pub cls: u8,
    pub fwd: bool,
}

pub struct LineCounter {
    lines: Vec<LineCfg>,
    counts: Vec<LineCounts>,
    counted: Vec<HashSet<u32>>,
    last_pos: HashMap<u32, ((f32, f32), u64)>,
    frame: u64,
    w: f32,
    h: f32,
}

fn cross(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    ax * by - ay * bx
}

/// Does segment p->q intersect segment a->b ?
fn intersects(p: (f32, f32), q: (f32, f32), a: (f32, f32), b: (f32, f32)) -> bool {
    let d1 = cross(b.0 - a.0, b.1 - a.1, p.0 - a.0, p.1 - a.1);
    let d2 = cross(b.0 - a.0, b.1 - a.1, q.0 - a.0, q.1 - a.1);
    let d3 = cross(q.0 - p.0, q.1 - p.1, a.0 - p.0, a.1 - p.1);
    let d4 = cross(q.0 - p.0, q.1 - p.1, b.0 - p.0, b.1 - p.1);
    d1 * d2 < 0.0 && d3 * d4 < 0.0
}

impl LineCounter {
    pub fn new(lines: Vec<LineCfg>, w: f32, h: f32) -> Self {
        let mut c = Self { lines: vec![], counts: vec![], counted: vec![], last_pos: HashMap::new(), frame: 0, w, h };
        c.reconfigure(lines);
        c
    }

    /// Apply an edited line set: lines keeping their id and geometry keep their counts.
    pub fn reconfigure(&mut self, lines: Vec<LineCfg>) {
        let mut counts = Vec::new();
        let mut counted = Vec::new();
        for l in &lines {
            let old = self.lines.iter().position(|o| o.id == l.id);
            match old {
                Some(i) if close(&self.lines[i], l) => {
                    let mut c = self.counts[i].clone();
                    c.name = l.name.clone();
                    counts.push(c);
                    counted.push(std::mem::take(&mut self.counted[i]));
                }
                _ => {
                    counts.push(LineCounts { id: l.id.clone(), name: l.name.clone(), ..Default::default() });
                    counted.push(HashSet::new());
                }
            }
        }
        self.lines = lines;
        self.counts = counts;
        self.counted = counted;
    }

    pub fn lines(&self) -> &[LineCfg] {
        &self.lines
    }

    pub fn counts(&self) -> &[LineCounts] {
        &self.counts
    }

    pub fn update(&mut self, tracks: &[TrackOut]) -> Vec<CrossEv> {
        self.frame += 1;
        let mut events = Vec::new();
        for t in tracks {
            let cur = ((t.bbox[0] + t.bbox[2]) / 2.0, (t.bbox[1] + t.bbox[3]) / 2.0);
            if let Some(&(prev, _)) = self.last_pos.get(&t.id) {
                for (i, l) in self.lines.iter().enumerate() {
                    if self.counted[i].contains(&t.id) {
                        continue;
                    }
                    let a = (l.x1 * self.w, l.y1 * self.h);
                    let b = (l.x2 * self.w, l.y2 * self.h);
                    if intersects(prev, cur, a, b) {
                        self.counted[i].insert(t.id);
                        let dir = cross(b.0 - a.0, b.1 - a.1, cur.0 - prev.0, cur.1 - prev.1);
                        let c = &mut self.counts[i];
                        if dir > 0.0 {
                            c.fwd += 1;
                        } else {
                            c.rev += 1;
                        }
                        *c.by_class.entry(class_name(t.cls).to_string()).or_default() += 1;
                        events.push(CrossEv { line_id: l.id.clone(), line_name: l.name.clone(), track: t.id, cls: t.cls, fwd: dir > 0.0 });
                    }
                }
            }
            self.last_pos.insert(t.id, (cur, self.frame));
        }
        if self.frame % 120 == 0 {
            let f = self.frame;
            self.last_pos.retain(|_, v| f - v.1 < 150);
        }
        events
    }
}

fn close(a: &LineCfg, b: &LineCfg) -> bool {
    (a.x1 - b.x1).abs() + (a.y1 - b.y1).abs() + (a.x2 - b.x2).abs() + (a.y2 - b.y2).abs() < 0.02
}
