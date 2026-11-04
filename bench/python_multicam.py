"""
Benchmark of the ORIGINAL Python pipeline (repo's StreamManager / PredictorThread / ModelManager /
BYTETracker / supervision LineZone, all unchanged) with N simulated 30 fps camera streams.

usage (from the repo root):
    python /path/to/chira/bench/python_multicam.py --cams 2 --fps 30 --seconds 40 [--size 544x960] [--model yolox_s]

Differences from production, on purpose: DB / webhook HTTP calls are stubbed out (no backend is running),
which can only make Python look *faster* than it really is.
The stream: an ffmpeg process (real time, -re) re-encodes the clip to H.264/MPEG-TS and serves it on a
local TCP port; cv2.VideoCapture connects to it exactly like it would to an RTSP camera.
"""
import argparse, os, subprocess, sys, threading, time
sys.path.insert(0, os.getcwd())

ap = argparse.ArgumentParser()
ap.add_argument("--cams", type=int, default=2)
ap.add_argument("--fps", type=int, default=30)
ap.add_argument("--seconds", type=int, default=40)
ap.add_argument("--size", default="544x960")          # model input HxW
ap.add_argument("--model", default="yolox_s")          # yolox_s | yolox_m (weights path below)
ap.add_argument("--weights", default="yolox_s.pth")
args = ap.parse_args()
VIDEO = os.environ.get("VIDEO", "videos/demo.mp4")
H, W = map(int, args.size.split("x"))

import torch
_orig_load = torch.load
torch.load = lambda p, *a, **k: _orig_load(args.weights if str(p).endswith("yolox_m.pth") else p, *a, **k)

import services.tracking_service.model_manager as mm
from services.yoloX.yolox.exp.screw_exp import Exp as BaseExp
class Exp(BaseExp):
    def __init__(self):
        super().__init__()
        self.depth, self.width = (0.33, 0.50) if args.model == "yolox_s" else (0.67, 0.75)
        self.test_size = (H, W)
        self.test_conf = 0.35
mm.Exp = Exp

# no backend running: stub the db/webhook calls
import services.tracking_service.inference_functions as inf
inf.asyncio_manager_for_line_events = lambda *a, **k: None
inf.asyncio_manager_for_polygon_events = lambda *a, **k: None

# instrumentation (counts only, behaviour unchanged)
import services.tracking_service.background_orchestrator as bo
from services.tracking_service.frame_getter import OutputFrame

class Stat:
    def __init__(self): self.src = 0; self.proc = set(); self.runs = 0; self.lat = []; self.lock = threading.Lock()
all_frames = []

def counting_get_frame(vcap, output_frame, stop_event, wait_ms):
    st = output_frame.stat
    while not stop_event.is_set():
        ret, frame = vcap.read()
        if ret:
            with output_frame.lock:
                output_frame.frame = frame
                st.src += 1
                output_frame.seq = st.src
                output_frame.ts = time.perf_counter()
        else:
            break
bo.get_frame = counting_get_frame

orig_init = OutputFrame.__init__
def patched_init(self):
    orig_init(self); self.stat = Stat(); self.seq = 0; self.ts = 0.0; all_frames.append(self)
OutputFrame.__init__ = patched_init

orig_process = inf.PredictorThread.process_frame
def counting_process(self, frame, frame_id, timer):
    of = self.output_frame
    with of.lock:
        seq, ts = of.seq, of.ts
    out = orig_process(self, frame, frame_id, timer)
    st = of.stat
    with st.lock:
        st.runs += 1
        st.proc.add(seq)
        st.lat.append((time.perf_counter() - ts) * 1000)
    return out
inf.PredictorThread.process_frame = counting_process

# camera setups == the two setups of the Rust app (normalised -> 1920x1080 pixels)
def line(name, x1, y1, x2, y2): return {"type": "line", "name": name, "coordinates": [[int(x1*1920), int(y1*1080)], [int(x2*1920), int(y2*1080)]]}
SETUPS = [
    (0,  [line("Line A", .04, .62, .62, .62)]),
    (95, [line("Line B", .25, .30, .56, .30), line("Line C", 0, .78, .66, .70)]),
]
CLASSES = ["car", "motorcycle", "bus", "truck", "bicycle"]

pubs, mgrs, ports = [], [], []
for i in range(args.cams):
    start_s, zones = SETUPS[i % 2]
    port = 9300 + i
    enc = (["-c:v", "copy", "-bsf:v", "h264_mp4toannexb"] if args.fps == 30 else
           ["-vf", f"fps={args.fps}", "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-b:v", "12M", "-g", str(args.fps)])
    pubs.append(subprocess.Popen(
        ["ffmpeg", "-loglevel", "error", "-re", "-stream_loop", "-1", "-ss", str(start_s), "-i", VIDEO, "-an", *enc,
         "-f", "mpegts", f"tcp://127.0.0.1:{port}?listen=1"],
        stdin=subprocess.DEVNULL))
    ports.append(port)
time.sleep(1.5)

# patch cv2.VideoCapture so the ffmpeg-listening publishers are reached by URL like an rtsp:// camera
for i in range(args.cams):
    start_s, zones = SETUPS[i % 2]
    m = bo.StreamManager(f"tcp://127.0.0.1:{ports[i]}", f"bench_{i}", zones, CLASSES, [], {})
    m.start_stream()
    mgrs.append(m)

# warm-up: model load + cuDNN init per camera, wait until every camera is processing
t0 = time.time()
while time.time() - t0 < 180:
    time.sleep(1)
    if len(all_frames) == args.cams and all(f.stat.runs >= 60 for f in all_frames):
        break
print(f"warm-up done after {time.time()-t0:.0f}s", flush=True)

def gpu():
    o = subprocess.run(["nvidia-smi", "--query-gpu=utilization.gpu", "--format=csv,noheader,nounits"], capture_output=True, text=True).stdout
    return float(o.split()[0])

def cpu_s():
    t = os.times(); return t.user + t.system

def snap():
    out = []
    for f in all_frames:
        st = f.stat
        with st.lock:
            out.append((st.src, len(st.proc), st.runs, len(st.lat)))
    return out

lat_start = [len(f.stat.lat) for f in all_frames]
base = snap(); c0 = cpu_s(); tb = time.perf_counter(); gpus = []; prev = base; tp = tb
while time.perf_counter() - tb < args.seconds:
    time.sleep(5)
    gpus.append(gpu())
    cur = snap(); now = time.perf_counter(); dt = now - tp
    line_ = " | ".join(f"cam{i}: src {(c[0]-p_[0])/dt:5.1f} proc {(c[1]-p_[1])/dt:5.1f} fps" for i, (c, p_) in enumerate(zip(cur, prev)))
    print(f"[{now-tb:4.0f}s] {line_} | GPU {gpus[-1]:.0f}%", flush=True)
    prev, tp = cur, now
end = snap(); dt = time.perf_counter() - tb
print("\n==== SUMMARY: python, %d cams @ %d fps source, %s %s ====" % (args.cams, args.fps, args.model, args.size))
for i, (c, b) in enumerate(zip(end, base)):
    src = (c[0] - b[0]) / dt; proc = (c[1] - b[1]) / dt; runs = (c[2] - b[2]) / dt
    lats = sorted(all_frames[i].stat.lat[lat_start[i]:])
    drop = 100 * (1 - (c[1] - b[1]) / max(1, c[0] - b[0]))
    print(f"cam{i}: source {src:5.1f} fps | processed(unique) {proc:5.1f} fps | dropped {drop:4.1f}% | "
          f"loop iterations {runs:5.1f}/s | latency p50 {lats[len(lats)//2]:.0f} ms p95 {lats[int(len(lats)*.95)]:.0f} ms")
print(f"GPU util avg {sum(gpus)/len(gpus):.0f}% | process CPU {(cpu_s()-c0)/dt:.1f} cores (excl. ffmpeg)")
for m in mgrs: m.stop_event.set()
for pb in pubs: pb.kill()
sys.stdout.flush()
os._exit(0)
