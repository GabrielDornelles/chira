//! ByteTrack in Rust (Kalman filter + two-stage IoU association).
//! Association is greedy-by-IoU instead of Hungarian: for road traffic with
//! well-separated boxes the result is practically identical and much simpler.

use std::collections::VecDeque;

#[derive(Clone, Copy, Debug)]
pub struct Det {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub score: f32,
    pub cls: u8,
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let ix1 = a[0].max(b[0]);
    let iy1 = a[1].max(b[1]);
    let ix2 = a[2].min(b[2]);
    let iy2 = a[3].min(b[3]);
    let inter = (ix2 - ix1).max(0.0) * (iy2 - iy1).max(0.0);
    let ua = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter;
    if ua <= 0.0 {
        0.0
    } else {
        inter / ua
    }
}

// ---------------- Kalman filter (state: x y a h vx vy va vh) ----------------
type M8 = [[f64; 8]; 8];

#[derive(Clone)]
struct Kalman {
    mean: [f64; 8],
    cov: M8,
}

const STD_POS: f64 = 1.0 / 20.0;
const STD_VEL: f64 = 1.0 / 160.0;

fn invert4(m: [[f64; 4]; 4]) -> [[f64; 4]; 4] {
    let mut a = m;
    let mut inv = [[0.0; 4]; 4];
    for i in 0..4 {
        inv[i][i] = 1.0;
    }
    for c in 0..4 {
        let mut p = c;
        for r in c + 1..4 {
            if a[r][c].abs() > a[p][c].abs() {
                p = r;
            }
        }
        a.swap(c, p);
        inv.swap(c, p);
        let d = a[c][c];
        let d = if d.abs() < 1e-12 { 1e-12 } else { d };
        for k in 0..4 {
            a[c][k] /= d;
            inv[c][k] /= d;
        }
        for r in 0..4 {
            if r != c {
                let f = a[r][c];
                for k in 0..4 {
                    a[r][k] -= f * a[c][k];
                    inv[r][k] -= f * inv[c][k];
                }
            }
        }
    }
    inv
}

impl Kalman {
    fn new(m: [f64; 4]) -> Self {
        let h = m[3];
        let mut mean = [0.0; 8];
        mean[..4].copy_from_slice(&m);
        let std = [
            2.0 * STD_POS * h,
            2.0 * STD_POS * h,
            1e-2,
            2.0 * STD_POS * h,
            10.0 * STD_VEL * h,
            10.0 * STD_VEL * h,
            1e-5,
            10.0 * STD_VEL * h,
        ];
        let mut cov = [[0.0; 8]; 8];
        for i in 0..8 {
            cov[i][i] = std[i] * std[i];
        }
        Self { mean, cov }
    }

    fn predict(&mut self) {
        let h = self.mean[3];
        let q = [
            STD_POS * h,
            STD_POS * h,
            1e-2,
            STD_POS * h,
            STD_VEL * h,
            STD_VEL * h,
            1e-5,
            STD_VEL * h,
        ];
        for i in 0..4 {
            self.mean[i] += self.mean[i + 4];
        }
        // P = F P F^T + Q, with F = [[I, I],[0, I]]
        let mut fp = self.cov;
        for i in 0..4 {
            for j in 0..8 {
                fp[i][j] += self.cov[i + 4][j];
            }
        }
        let mut out = fp;
        for j in 0..4 {
            for i in 0..8 {
                out[i][j] += fp[i][j + 4];
            }
        }
        for i in 0..8 {
            out[i][i] += q[i] * q[i];
        }
        self.cov = out;
    }

    fn update(&mut self, z: [f64; 4]) {
        let h = self.mean[3];
        let r = [STD_POS * h, STD_POS * h, 1e-1, STD_POS * h];
        let mut s = [[0.0; 4]; 4];
        for i in 0..4 {
            for j in 0..4 {
                s[i][j] = self.cov[i][j];
            }
            s[i][i] += r[i] * r[i];
        }
        let si = invert4(s);
        // K = P H^T S^-1 (P H^T = first 4 columns of P)
        let mut k = [[0.0; 4]; 8];
        for i in 0..8 {
            for j in 0..4 {
                for l in 0..4 {
                    k[i][j] += self.cov[i][l] * si[l][j];
                }
            }
        }
        let mut innov = [0.0; 4];
        for i in 0..4 {
            innov[i] = z[i] - self.mean[i];
        }
        for i in 0..8 {
            for j in 0..4 {
                self.mean[i] += k[i][j] * innov[j];
            }
        }
        // P = P - K S K^T  (= P - K (P H^T)^T)
        let mut newp = self.cov;
        for i in 0..8 {
            for j in 0..8 {
                let mut acc = 0.0;
                for l in 0..4 {
                    acc += k[i][l] * self.cov[j][l];
                }
                newp[i][j] -= acc;
            }
        }
        self.cov = newp;
    }

    fn tlbr(&self) -> [f32; 4] {
        let (x, y, a, h) = (self.mean[0], self.mean[1], self.mean[2], self.mean[3]);
        let w = a * h;
        [
            (x - w / 2.0) as f32,
            (y - h / 2.0) as f32,
            (x + w / 2.0) as f32,
            (y + h / 2.0) as f32,
        ]
    }
}

fn to_xyah(b: &[f32; 4]) -> [f64; 4] {
    let w = (b[2] - b[0]) as f64;
    let h = (b[3] - b[1]) as f64;
    [b[0] as f64 + w / 2.0, b[1] as f64 + h / 2.0, w / h.max(1e-6), h]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Tracked,
    Lost,
}

struct Track {
    id: u32,
    kf: Kalman,
    state: State,
    confirmed: bool,
    score: f32,
    cls: u8,
    last_frame: u64,
    trail: VecDeque<(f32, f32)>,
}

impl Track {
    fn bbox(&self) -> [f32; 4] {
        self.kf.tlbr()
    }
    fn apply(&mut self, d: &Det, frame: u64) {
        self.kf.update(to_xyah(&[d.x1, d.y1, d.x2, d.y2]));
        self.score = d.score;
        self.cls = d.cls;
        self.state = State::Tracked;
        self.last_frame = frame;
        let b = self.bbox();
        self.trail.push_back(((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0));
        if self.trail.len() > 40 {
            self.trail.pop_front();
        }
    }
}

#[derive(Clone, Debug)]
pub struct TrackOut {
    pub id: u32,
    pub bbox: [f32; 4],
    pub cls: u8,
    pub score: f32,
}

pub struct ByteTracker {
    tracks: Vec<Track>,
    next_id: u32,
    frame: u64,
    pub high_thresh: f32,
    pub low_thresh: f32,
    pub max_lost: u64,
}

/// Greedy matching on IoU. Returns (matches (track_idx, det_idx), unmatched tracks, unmatched dets).
fn associate(
    track_boxes: &[[f32; 4]],
    dets: &[Det],
    min_iou: f32,
) -> (Vec<(usize, usize)>, Vec<usize>, Vec<usize>) {
    let mut cands: Vec<(f32, usize, usize)> = Vec::new();
    for (ti, tb) in track_boxes.iter().enumerate() {
        for (di, d) in dets.iter().enumerate() {
            let v = iou(tb, &[d.x1, d.y1, d.x2, d.y2]);
            if v > min_iou {
                cands.push((v, ti, di));
            }
        }
    }
    cands.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let mut tu = vec![true; track_boxes.len()];
    let mut du = vec![true; dets.len()];
    let mut matches = Vec::new();
    for (_, ti, di) in cands {
        if tu[ti] && du[di] {
            tu[ti] = false;
            du[di] = false;
            matches.push((ti, di));
        }
    }
    let ut = (0..tu.len()).filter(|&i| tu[i]).collect();
    let ud = (0..du.len()).filter(|&i| du[i]).collect();
    (matches, ut, ud)
}

impl ByteTracker {
    pub fn new() -> Self {
        Self {
            tracks: Vec::new(),
            next_id: 1,
            frame: 0,
            high_thresh: 0.5,
            low_thresh: 0.1,
            max_lost: 30,
        }
    }

    pub fn update(&mut self, dets: &[Det]) -> Vec<TrackOut> {
        self.frame += 1;
        let frame = self.frame;
        for t in &mut self.tracks {
            t.kf.predict();
        }
        let high: Vec<Det> = dets.iter().filter(|d| d.score >= self.high_thresh).copied().collect();
        let low: Vec<Det> = dets
            .iter()
            .filter(|d| d.score >= self.low_thresh && d.score < self.high_thresh)
            .copied()
            .collect();

        // Stage 1: confirmed tracks (tracked + lost) vs high-score detections
        let pool: Vec<usize> = (0..self.tracks.len()).filter(|&i| self.tracks[i].confirmed).collect();
        let boxes: Vec<[f32; 4]> = pool.iter().map(|&i| self.tracks[i].bbox()).collect();
        let (m1, ut1, ud1) = associate(&boxes, &high, 0.2);
        for (pi, di) in m1 {
            self.tracks[pool[pi]].apply(&high[di], frame);
        }

        // Stage 2: still-unmatched *tracked* tracks vs low-score detections
        let rem: Vec<usize> = ut1
            .iter()
            .map(|&p| pool[p])
            .filter(|&i| self.tracks[i].state == State::Tracked)
            .collect();
        let rboxes: Vec<[f32; 4]> = rem.iter().map(|&i| self.tracks[i].bbox()).collect();
        let (m2, ut2, _) = associate(&rboxes, &low, 0.5);
        for (ri, di) in m2 {
            self.tracks[rem[ri]].apply(&low[di], frame);
        }
        for ri in ut2 {
            self.tracks[rem[ri]].state = State::Lost;
        }

        // Stage 3: unconfirmed tracks vs leftover high detections
        let left: Vec<Det> = ud1.iter().map(|&i| high[i]).collect();
        let unc: Vec<usize> = (0..self.tracks.len()).filter(|&i| !self.tracks[i].confirmed).collect();
        let uboxes: Vec<[f32; 4]> = unc.iter().map(|&i| self.tracks[i].bbox()).collect();
        let (m3, ut3, ud3) = associate(&uboxes, &left, 0.3);
        for (ui, di) in m3 {
            let t = &mut self.tracks[unc[ui]];
            t.apply(&left[di], frame);
            t.confirmed = true;
        }
        let mut dead: Vec<usize> = ut3.iter().map(|&u| unc[u]).collect();

        // New tracks from detections nobody claimed
        for di in ud3 {
            let d = left[di];
            if d.score < self.high_thresh + 0.1 {
                continue;
            }
            let mut t = Track {
                id: self.next_id,
                kf: Kalman::new(to_xyah(&[d.x1, d.y1, d.x2, d.y2])),
                state: State::Tracked,
                confirmed: frame == 1,
                score: d.score,
                cls: d.cls,
                last_frame: frame,
                trail: VecDeque::new(),
            };
            self.next_id += 1;
            let b = t.bbox();
            t.trail.push_back(((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0));
            self.tracks.push(t);
        }

        // Drop long-lost tracks
        for (i, t) in self.tracks.iter().enumerate() {
            if t.state == State::Lost && frame - t.last_frame > self.max_lost {
                dead.push(i);
            }
        }
        dead.sort_unstable();
        dead.dedup();
        for i in dead.into_iter().rev() {
            self.tracks.swap_remove(i);
        }

        self.tracks
            .iter()
            .filter(|t| t.confirmed && t.state == State::Tracked && t.last_frame == frame)
            .map(|t| TrackOut { id: t.id, bbox: t.bbox(), cls: t.cls, score: t.score })
            .collect()
    }
}
