# Research notes: speed, models, trackers

Everything here was measured on one machine (RTX 2060 12 GB, 12 CPU cores) with one traffic clip, so treat the *ranking* as the
takeaway, not the absolute numbers. Scripts live in `research/` and `bench/`.

## Batching

* One shared ORT session; batch shape = number of cameras (capped by `MAX_BATCH`), padded when a camera has no
  fresh frame, waiting at most `BATCH_WAIT_MS` for all cameras. **Changing the input shape between calls costs
  ~150 ms in ORT/cuDNN (measured: `./target/release/chira --bench-model`)**, so the shape is only
  changed when a camera is added/removed.
* GPU cost per frame (RTX 2060, fp16, 544x960): batch 1 = 7.3 ms, batch 8 = 4.6 ms/frame (~1.6x).

## Python vs Rust (RTX 2060, 12 cores, 30 fps real-time sources, same clip, YOLOX-S fp16 544x960, ByteTrack, line counting)

| cameras | Python (original pipeline) fps/cam, dropped | Rust, 1 frame per GPU call | Rust, batched (wait 40 ms) |
|---|---|---|---|
| 1 | 27.9, 7%  | - | - |
| 2 | 25.2, 16% | 30.0, 0% | 29.8, 0.7% |
| 3 | 22.1, 26% | 30.0, 0.1% | 29.8, 0.7% |
| 5 | -         | 29.9, 0.3% | 29.4, 1.9% |
| 6 | -         | 27.2, 9.4% | 29.1, 3.0% |
| 8 | -         | 19.7, 34% | 24.3, 19% (194 fps total) |

Reproduce: `bench/rust_matrix.sh 30 "1 8:40" "2 3 5 6 8"` and `python bench/python_multicam.py --cams 3 --fps 30`.
The Python harness stubs the DB/webhook calls and handles full-resolution 1080p frames (as the app does);
the Rust app downscales to 960x540 in ffmpeg before inference.

## Models and trackers (Nov 2026)

Everything measured on this machine (RTX 2060, 544x960 input, fp16) with this clip. Scripts are in `research/`.

**Accuracy** = COCO AP of "vehicle" boxes on 204 frames vs. pseudo-ground-truth from two strong references run on the full
1080p frames (YOLO26-X @1088x1920 and RF-DETR-L); the table uses the boxes both references agree on, so neither family is favoured.
No human labels: absolute numbers are relative-to-reference, the ranking is what matters.

| model | licence | AP50:95 | AP50 | AP small | GPU ms/frame (ORT CUDA, batch 4) |
|---|---|---|---|---|---|
| YOLOX-S (current) | Apache-2.0 | 78.0 | 95.2 | 72.6 | 5.9 |
| YOLOX-M (2024 config) | Apache-2.0 | 85.6 | 96.6 | 79.4 | 13.5 |
| YOLO26-N | AGPL-3.0 | 79.3 | 95.5 | 71.3 | 3.2 |
| **YOLO26-S** | AGPL-3.0 | **87.6** | 96.7 | 82.7 | 5.9 |
| **YOLO26-M** | AGPL-3.0 | **91.5** | 97.4 | 87.4 | 11.8 |
| YOLO26-L | AGPL-3.0 | 91.6 | 96.9 | 88.8 | 14.7 |
| YOLO26-X | AGPL-3.0 | 93.1 | 97.2 | 90.9 | 29.2 |
| YOLO11-S / M | AGPL-3.0 | 87.1 / 90.0 | 96.7 / 97.1 | 82.4 / 85.5 | 5.7 / 11.8 |
| RF-DETR N / S / M / L | Apache-2.0 | 62.8 / 70.9 / 77.1 / 76.0 | 94.5 / 96.1 / 97.1 / 97.1 | 52.9 / 59.3 / 66.7 / 64.7 | ~40 on CUDA EP; 7-10 on TensorRT |

* YOLO26-S has the speed of YOLOX-S with the accuracy of (better than) YOLOX-M; YOLO26-M is +6 AP over YOLOX-M at lower cost.
* RF-DETR finds the same vehicles (AP50 on par) but its boxes are looser at this non-native 544x960 input; it only becomes fast under TensorRT.
* **TensorRT (`EP=trt`) is ~2x faster than the CUDA provider** (YOLO26-S: 2.5 vs 5.1 ms/frame at batch 4). First start builds one engine per batch size (minutes, cached in `trt_cache/`).

Multi-camera, 30 fps real-time sources, batched (`MAX_BATCH=8`), processed fps per camera / frames dropped:

| setup | 5 cams | 6 cams | 8 cams |
|---|---|---|---|
| YOLOX-S, CUDA EP | 29.4 / 2% | 27.4 / 9% | 22.5 / 25% |
| YOLO26-S, CUDA EP | 28.6 / 5% | 24.9 / 17% | 20.6 / 31% |
| **YOLO26-S, TensorRT** | - | **29.6 / 1%** | **28.9 / 3.5%** (GPU 56%) |
| **YOLO26-M, TensorRT** | 4 cams: **29.9 / 0.6%** | **29.7 / 1.2%** (GPU 76%) | - |
| YOLOX-M, CUDA EP | 13.5 / 55% | 11.2 / 63% | 9.5 / 68% |
| YOLO26-M, CUDA EP | 14.5 / 52% | 13.1 / 56% | 11.3 / 62% |

**Trackers** (same YOLO26-M detections, 60 s / 1800 frames, 3 counting lines, static camera, no ReID):
crossings counted ByteTrack 177, BoostTrack 177, BoT-SORT 175, HybridSORT 173, OC-SORT 167, SFSORT 166. The 9 crossings only the
first group counted were checked by eye (3 of 3 inspected were real vehicles), so ByteTrack/BoostTrack/BoT-SORT are the most complete and **ByteTrack
(already in use) is as good as the newer ones here** at ~1.8 ms/frame on CPU. No reason to switch for fixed traffic cameras.

### Using the new models

```
# export once (see the README quick start), then:
MODEL=models/yolo26s_544x960.onnx ./run.sh                       # CUDA provider
EP=trt TRT_LIBS=/path/to/tensorrt_libs MODEL=models/yolo26m_544x960.onnx ./run.sh   # TensorRT: ~2x faster
```
TensorRT libs: `pip install tensorrt-cu12-libs==10.9.0.34` (then `TRT_LIBS=<venv>/lib/python3.12/site-packages/tensorrt_libs`).
First start with `EP=trt` builds engines for batch sizes 1..`MAX_BATCH` (about 3-5 min each, cached in `trt_cache/`).
`research/export_ultra_wrapped.py weights.pt out.onnx` produces the ONNX from any Ultralytics weights.
Licence note: YOLO26/YOLO11 are AGPL-3.0 (fine for this experiment; YOLOX and RF-DETR N-L are Apache-2.0).


