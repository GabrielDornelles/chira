"""Tracker comparison on identical detections.  usage: track_eval.py WORKDIR DETECTOR_ONNX START_S SECONDS
1) run the detector on every frame of the clip (30 fps, 960x540) and cache the detections
2) feed the same detections to each boxmot tracker, count line crossings (same rule as the Rust app: once per ID per line)
3) cluster crossing events across trackers (time +-0.7 s, x +-70 px) -> consensus; report each tracker vs consensus."""
import sys, os, subprocess, json, time, glob
import numpy as np, cv2, onnxruntime as ort, torch, torchvision
work, onnx, start, secs = sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4])
VIDEO = os.environ.get("VIDEO", "videos/demo.mp4")
W, H, VH = 960, 544, 540
VEH = [1, 2, 3, 5, 7]
LINES = {"B": (.25, .30, .56, .30), "C": (0, .78, .66, .70), "A": (.04, .62, .62, .62)}
name = os.path.basename(onnx)[:-5]
cache = f"{work}/dets2_{name}_{int(start)}_{int(secs)}.npz"

def frames():
    p = subprocess.Popen(["ffmpeg", "-loglevel", "error", "-ss", str(start), "-t", str(secs), "-i", VIDEO, "-vf", f"scale={W}:{VH}", "-pix_fmt", "bgr24", "-f", "rawvideo", "-"], stdout=subprocess.PIPE)
    while True:
        b = p.stdout.read(W * VH * 3)
        if len(b) < W * VH * 3: break
        yield np.frombuffer(b, np.uint8).reshape(VH, W, 3).copy()

imgs = list(frames()); print(len(imgs), "frames", flush=True)
if os.path.exists(cache):
    dets = list(np.load(cache, allow_pickle=True)["dets"])
else:
    s = ort.InferenceSession(onnx, providers=["CUDAExecutionProvider"]); inp = s.get_inputs()[0]; dets = []
    for im in imgs:
        pad = np.full((H, W, 3), 114, np.uint8); pad[:VH] = im[:, :, ::-1]
        if inp.type == "tensor(uint8)": x = pad[None]
        else:
            x = (pad.transpose(2, 0, 1)[None].astype(np.float32) / 255); x = x.astype(np.float16) if inp.type == "tensor(float16)" else x
        o = s.run(None, {inp.name: x})[0][0].astype(np.float32)
        if o.shape[-1] == 6:                                              # [300,6] x1 y1 x2 y2 score cls  (yolox wrapper: pre-NMS)
            o = o[(o[:, 4] >= 0.1) & np.isin(o[:, 5].astype(int), VEH)]
        else:                                                             # ultralytics raw [84, N]: cx cy w h + 80 class scores
            o = o.T; cls = o[:, 4:]; sc = cls[:, VEH].max(1); ci = np.array(VEH)[cls[:, VEH].argmax(1)]; k = sc >= 0.1
            o = np.c_[o[k, 0] - o[k, 2] / 2, o[k, 1] - o[k, 3] / 2, o[k, 0] + o[k, 2] / 2, o[k, 1] + o[k, 3] / 2, sc[k], ci[k]]
        if len(o):
            o = o[torchvision.ops.batched_nms(torch.tensor(o[:, :4]), torch.tensor(o[:, 4]), torch.tensor(o[:, 5]).long(), 0.5).numpy()]
        dets.append(o)
    np.savez(cache, dets=np.array(dets, dtype=object))
dets = [np.concatenate([d[:, :4], np.clip(d[:, 4:5], 0, 1), d[:, 5:6]], 1) if len(d) else d for d in dets]
print("detections ready: avg per frame", np.mean([len(d) for d in dets]), flush=True)

from boxmot import ByteTrack, OcSort, BotSort, HybridSort, BoostTrack, SFSORT, BotSortConfig, HybridSortConfig, BoostTrackConfig
def mk(cls, **kw):
    return lambda: cls(**kw)
import inspect
NOREID = {  # appearance-free configs: static camera -> no camera-motion compensation, no ReID network
    BotSort: lambda: BotSort(BotSortConfig(use_embeddings=False, use_cmc=False)),
    HybridSort: lambda: HybridSort(HybridSortConfig(use_embeddings=False, use_cmc=False, with_longterm_reid=False, with_longterm_reid_correction=False)),
    BoostTrack: lambda: BoostTrack(BoostTrackConfig(use_embeddings=False, use_cmc=False)),
}
def make(cls):
    if cls in NOREID: return NOREID[cls]()
    sig = inspect.signature(cls.__init__).parameters
    kw = {}
    if "reid_weights" in sig: kw["reid_weights"] = None
    if "with_reid" in sig: kw["with_reid"] = False
    if "device" in sig: kw["device"] = "cpu"
    if "half" in sig: kw["half"] = False
    if "per_class" in sig: kw["per_class"] = False
    if "fps" in sig: kw["fps"] = 30
    if "frame_rate" in sig: kw["frame_rate"] = 30
    return cls(**kw)
TR = {"ByteTrack": ByteTrack, "OC-SORT": OcSort, "BoT-SORT": BotSort, "SFSORT": SFSORT, "HybridSORT": HybridSort, "BoostTrack": BoostTrack}

def crossings(tracks_by_frame):
    last, done, ev = {}, set(), []
    for t, rows in enumerate(tracks_by_frame):
        for r in rows:
            tid = int(r[4]); c = ((r[0] + r[2]) / 2, (r[1] + r[3]) / 2)
            if tid in last:
                p = last[tid]
                for ln, (x1, y1, x2, y2) in LINES.items():
                    if (tid, ln) in done: continue
                    a, b = (x1 * W, y1 * VH), (x2 * W, y2 * VH)
                    cr = lambda ax, ay, bx, by: ax * by - ay * bx
                    d1 = cr(b[0]-a[0], b[1]-a[1], p[0]-a[0], p[1]-a[1]); d2 = cr(b[0]-a[0], b[1]-a[1], c[0]-a[0], c[1]-a[1])
                    d3 = cr(c[0]-p[0], c[1]-p[1], a[0]-p[0], a[1]-p[1]); d4 = cr(c[0]-p[0], c[1]-p[1], b[0]-p[0], b[1]-p[1])
                    if d1 * d2 < 0 and d3 * d4 < 0:
                        done.add((tid, ln)); ev.append((t / 30, c[0], ln, tid))
            last[tid] = c
    return ev

results = {}
for tname, cls in TR.items():
    try:
        tr = make(cls); out = []; t0 = time.perf_counter()
        for im, d in zip(imgs, dets):
            o = tr.update(d.reshape(-1, 6) if len(d) else np.empty((0, 6)), im)
            out.append(np.asarray(o).reshape(-1, 8) if len(o) else np.empty((0, 8)))
        dt = (time.perf_counter() - t0) / len(imgs) * 1000
        ids = {int(r[4]) for rows in out for r in rows}
        lens = {}
        for rows in out:
            for r in rows: lens[int(r[4])] = lens.get(int(r[4]), 0) + 1
        results[tname] = dict(ev=crossings(out), ids=len(ids), mean_len=float(np.mean(list(lens.values()))), ms=dt)
        print(f"{tname:11s} ok  ids {len(ids):4d}  mean track length {np.mean(list(lens.values())):6.1f} frames  {dt:5.2f} ms/frame  crossings {len(results[tname]['ev'])}", flush=True)
    except Exception as e:
        import traceback; traceback.print_exc(); print(tname, "FAILED", e, flush=True)

# consensus clusters
allev = [(t, x, ln, tn) for tn, r in results.items() for (t, x, ln, _) in r["ev"]]
clusters = []
for ev in sorted(allev):
    for c in clusters:
        if c["ln"] == ev[2] and abs(c["t"] - ev[0]) < 0.7 and abs(c["x"] - ev[1]) < 70:
            c["m"].append(ev); c["t"] = np.mean([m[0] for m in c["m"]]); c["x"] = np.mean([m[1] for m in c["m"]]); break
    else: clusters.append({"t": ev[0], "x": ev[1], "ln": ev[2], "m": [ev]})
n = len(results); consensus = [c for c in clusters if len({m[3] for m in c["m"]}) >= (n + 1) // 2 + (1 if n % 2 == 0 else 0)]
print(f"\nclusters {len(clusters)}, consensus (>= majority of {n} trackers agree): {len(consensus)}")
for tn in results:
    sup = [c for c in clusters if tn in {m[3] for m in c["m"]}]
    cons_hit = sum(1 for c in consensus if tn in {m[3] for m in c["m"]})
    dup = sum(len([m for m in c["m"] if m[3] == tn]) - 1 for c in clusters if tn in {m[3] for m in c["m"]})
    print(f"{tn:11s} counted {len(results[tn]['ev']):3d} | matches consensus {cons_hit}/{len(consensus)} (missed {len(consensus)-cons_hit}) | not in consensus {len(sup)-cons_hit} | double counts {dup} | ids {results[tn]['ids']} | {results[tn]['ms']:.2f} ms/frame")
json.dump({"clusters": [{"t": c["t"], "x": c["x"], "ln": c["ln"], "by": sorted({m[3] for m in c["m"]})} for c in clusters]}, open(f"{work}/clusters_{name}.json", "w"))
