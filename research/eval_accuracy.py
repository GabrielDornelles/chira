"""Accuracy of every candidate (ONNX, 544x960 input) against pseudo-GT from full-resolution reference detectors.
usage: eval_accuracy.py WORKDIR [model ...]   -> prints a table; caches per-model detections in WORKDIR/dets/"""
import glob, json, os, sys, io, contextlib
import cv2, numpy as np, onnxruntime as ort, torch, torchvision
from pycocotools.coco import COCO
from pycocotools.cocoeval import COCOeval
work = sys.argv[1]; names = sys.argv[2:]
os.makedirs(work + "/dets", exist_ok=True)
files = sorted(glob.glob(work + "/frames/*.jpg"))
VEH = [1, 2, 3, 5, 7]
W, H = 960, 544
refs = json.load(open(work + "/refs.json"))

def load(f):
    im = cv2.cvtColor(cv2.imread(f), cv2.COLOR_BGR2RGB)
    im = cv2.resize(im, (960, 540), interpolation=cv2.INTER_AREA)
    pad = np.full((H, W, 3), 114, np.uint8); pad[:540] = im
    return pad

def run_model(name):
    cache = f"{work}/dets/{name}.json"
    if os.path.exists(cache): return json.load(open(cache))
    so = ort.SessionOptions(); so.log_severity_level = 3
    s = ort.InferenceSession(f"{work}/onnx/{name}.onnx", so, providers=["CUDAExecutionProvider"])
    inp = s.get_inputs()[0]; res = {}
    for f in files:
        pad = load(f)
        if inp.type == "tensor(uint8)":
            x = pad[None]                                                    # yolox wrapper: NHWC uint8
        else:
            x = pad.transpose(2, 0, 1)[None].astype(np.float32) / 255
            if name.startswith("rfdetr"):
                x = (x - np.array([0.485, 0.456, 0.406], np.float32)[None, :, None, None]) / np.array([0.229, 0.224, 0.225], np.float32)[None, :, None, None]
            if inp.type == "tensor(float16)": x = x.astype(np.float16)
        outs = s.run(None, {inp.name: x})
        rows = []
        if name.startswith("rfdetr"):
            dets, logits = outs[0][0], outs[1][0]
            sc = 1 / (1 + np.exp(-logits.astype(np.float32)))
            for k, c80 in {2: 1, 3: 2, 4: 3, 6: 5, 8: 7}.items():
                pass
            c91 = {2: 1, 3: 2, 4: 3, 6: 5, 8: 7}
            for q in range(dets.shape[0]):
                best = max(((sc[q, k], c) for k, c in c91.items()), key=lambda t: t[0])
                cx, cy, w, h = dets[q] * np.array([W, H, W, H])
                rows.append([cx - w / 2, cy - h / 2, cx + w / 2, cy + h / 2, float(best[0]), best[1]])
        else:
            o = outs[0][0].astype(np.float32)
            if o.shape[-1] == 6:                                             # x1 y1 x2 y2 score cls
                if inp.type == "tensor(uint8)":                              # yolox wrapper: top-300 *before* NMS (the Rust app runs NMS 0.45)
                    o = o[o[:, 4] >= 0.05]
                    kk = torchvision.ops.batched_nms(torch.tensor(o[:, :4]), torch.tensor(o[:, 4]), torch.tensor(o[:, 5]).long(), 0.45).numpy()
                    o = o[kk]
                rows = [[*r[:4], r[4], int(r[5])] for r in o]
            else:                                                            # yolo11: [84, N] needs NMS
                o = o.T; boxes = o[:, :4]; cls = o[:, 4:]
                sc = cls[:, VEH].max(1); ci = np.array(VEH)[cls[:, VEH].argmax(1)]
                keep = sc > 0.05
                b = torch.tensor(np.c_[boxes[keep, 0] - boxes[keep, 2] / 2, boxes[keep, 1] - boxes[keep, 3] / 2, boxes[keep, 0] + boxes[keep, 2] / 2, boxes[keep, 1] + boxes[keep, 3] / 2])
                kk = torchvision.ops.batched_nms(b, torch.tensor(sc[keep]), torch.tensor(ci[keep]), 0.6)
                rows = [[*b[i].tolist(), float(sc[keep][i]), int(ci[keep][i])] for i in kk]
        res[os.path.basename(f)] = [r for r in rows if int(r[5]) in VEH and r[4] >= 0.05]
    json.dump(res, open(cache, "w"), default=float)
    return res

def iou(a, b):
    ix = np.maximum(0, np.minimum(a[2], b[:, 2]) - np.maximum(a[0], b[:, 0])); iy = np.maximum(0, np.minimum(a[3], b[:, 3]) - np.maximum(a[1], b[:, 1]))
    inter = ix * iy; return inter / ((a[2] - a[0]) * (a[3] - a[1]) + (b[:, 2] - b[:, 0]) * (b[:, 3] - b[:, 1]) - inter + 1e-9)

def gt_sets():
    A = {k: np.array([r for r in v if r[4] >= 0.5]).reshape(-1, 6) * [.5, .5, .5, .5, 1, 1] for k, v in refs["yolo26x_1920"].items()}
    B = {k: np.array([r for r in v if r[4] >= 0.5]).reshape(-1, 6) * [.5, .5, .5, .5, 1, 1] for k, v in refs["rfdetr_l_native"].items()}
    AB = {}
    for k in A:                                                              # agreed: A box confirmed by a B box (IoU>=0.6)
        keep = [a for a in A[k] if len(B[k]) and iou(a, B[k]).max() >= 0.6]
        AB[k] = np.array(keep).reshape(-1, 6)
    return {"ref A (YOLO26-X@1080p)": A, "ref B (RF-DETR-L)": B, "A∩B agreed": AB}

def coco_eval(dets, gt):
    images = [{"id": i, "width": 960, "height": 540} for i in range(len(files))]
    anns, preds, aid = [], [], 1
    for i, f in enumerate(files):
        k = os.path.basename(f)
        for g in gt.get(k, []):
            w, h = g[2] - g[0], g[3] - g[1]; anns.append({"id": aid, "image_id": i, "category_id": 1, "bbox": [g[0], g[1], w, h], "area": w * h, "iscrowd": 0}); aid += 1
        for d in dets.get(k, []):
            preds.append({"image_id": i, "category_id": 1, "bbox": [d[0], d[1], d[2] - d[0], d[3] - d[1]], "score": d[4]})
    c = COCO(); c.dataset = {"images": images, "annotations": anns, "categories": [{"id": 1}]}
    with contextlib.redirect_stdout(io.StringIO()):
        c.createIndex(); E = COCOeval(c, c.loadRes(preds) if preds else COCO(), "bbox"); E.evaluate(); E.accumulate(); E.summarize()
    return E.stats  # AP, AP50, AP75, APs, APm, APl, ...

def prf(dets, gt, thr):
    tp = fp = fn = 0
    for f in files:
        k = os.path.basename(f); g = np.array(gt.get(k, [])).reshape(-1, 6)
        d = sorted([x for x in dets.get(k, []) if x[4] >= thr], key=lambda x: -x[4]); used = set()
        for x in d:
            if len(g):
                v = iou(np.array(x), g[:, :4])
                if used: v[list(used)] = 0
                j = int(v.argmax())
                if v[j] >= 0.5: used.add(j); tp += 1; continue
            fp += 1
        fn += len(g) - len(used)
    p = tp / max(1, tp + fp); r = tp / max(1, tp + fn); return p, r, 2 * p * r / max(1e-9, p + r)

GT = gt_sets()
print(f"frames {len(files)}; GT boxes: " + ", ".join(f"{k}: {sum(len(v) for v in g.values())}" for k, g in GT.items()))
for name in names:
    dets = run_model(name)
    line = f"{name:11s}"
    for gname, g in GT.items():
        st = coco_eval(dets, g); p, r, f1 = prf(dets, g, 0.35)
        line += f" | {gname[:5]} AP {st[0]*100:4.1f} AP50 {st[1]*100:4.1f} APsmall {st[3]*100:4.1f} P/R@.35 {p:.2f}/{r:.2f}"
    print(line, flush=True)
