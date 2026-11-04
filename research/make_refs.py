"""Pseudo ground truth: two strong detectors run on the FULL 1080p frames (4x the pixels the candidates get).
 A = YOLO26-X @1088x1920 (PyTorch)   B = RF-DETR-L (PyTorch, native res)   -> refs.json {ref: {frame: [[x1,y1,x2,y2,score,cls],...]}}  (1080p pixels)"""
import glob, json, sys, os
import cv2, numpy as np
work = sys.argv[1]
files = sorted(glob.glob(work + "/frames/*.jpg"))
VEH = {1, 2, 3, 5, 7}                      # COCO-80 ids: bicycle car motorcycle bus truck
out = {"yolo26x_1920": {}, "rfdetr_l_native": {}}
from ultralytics import YOLO
m = YOLO(work + "/pt/yolo26x.pt")
for f in files:
    r = m.predict(f, imgsz=(1088, 1920), half=True, conf=0.2, classes=sorted(VEH), verbose=False, device=0)[0]
    b = r.boxes
    out["yolo26x_1920"][os.path.basename(f)] = np.c_[b.xyxy.cpu().numpy(), b.conf.cpu().numpy(), b.cls.cpu().numpy()].tolist()
import rfdetr
rf = rfdetr.RFDETRLarge()
COCO91_TO_80 = {2: 1, 3: 2, 4: 3, 6: 5, 8: 7}   # bicycle car motorcycle bus truck
for f in files:
    img = cv2.cvtColor(cv2.imread(f), cv2.COLOR_BGR2RGB)
    d = rf.predict(img, threshold=0.2)
    rows = [[*xy, c, COCO91_TO_80[int(k)]] for xy, c, k in zip(d.xyxy.tolist(), d.confidence.tolist(), d.class_id.tolist()) if int(k) in COCO91_TO_80]
    out["rfdetr_l_native"][os.path.basename(f)] = rows
json.dump(out, open(work + "/refs.json", "w"))
print("refs done", {k: sum(len(v) for v in d.values()) for k, d in out.items()}, flush=True)
