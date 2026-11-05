"""Ultralytics model -> ONNX with the SAME interface as the YOLOX export used by the Rust app:
   input  uint8 [B,H,W,3] RGB (dynamic batch, H x W fixed per file: default 544x960)    output  fp32 [B,300,6]  x1 y1 x2 y2 score cls(COCO-80)
Layout/scale conversion, class filter (the 15 classes in src/config.rs: person, vehicles, animals, bags) and top-300 run on the GPU; NMS stays on the CPU.
usage: export_ultra_wrapped.py weights.pt out.onnx [e2e] [--imgsz H W]
   Bigger input = smaller objects become findable (cameras mounted high / far away), at the cost of GPU time.
   Both sides must be multiples of 32 and the picture is 16:9, so: 544 960 (default) · 736 1280 · 1088 1920.
   Name the file <model>_<H>x<W>.onnx so Chira can list it, e.g. models/yolo26m_736x1280.onnx"""
import sys, torch
from ultralytics import YOLO
args = sys.argv[1:]
IMGSZ = (544, 960)
if "--imgsz" in args:
    i = args.index("--imgsz"); IMGSZ = (int(args[i + 1]), int(args[i + 2])); del args[i:i + 3]
assert IMGSZ[0] % 32 == 0 and IMGSZ[1] % 32 == 0, "H and W must be multiples of 32"
pt, out = args[0], args[1]
det = YOLO(pt).model.fuse().eval()
KEEP = [0, 2, 7, 5, 3, 1, 6, 8, 16, 15, 14, 17, 24, 26, 28]   # keep in sync with CLASSES in src/config.rs
E2E = len(args) > 2 and args[2] == "e2e"
head = det.model[-1]; head.export = True
head.end2end = E2E   # e2e: YOLO26 one2one head (trained NMS-free, ~300 clean boxes); otherwise one2many head + external NMS
for p in det.parameters(): p.requires_grad = False

class Wrapper(torch.nn.Module):
    def __init__(self, m):
        super().__init__(); self.m = m.half()
        self.register_buffer("keep", torch.tensor(KEEP))
    def forward(self, x):
        x = x.permute(0, 3, 1, 2).half() / 255                       # uint8 NHWC RGB -> fp16 NCHW 0..1
        y = self.m(x)
        y = (y[0] if isinstance(y, (tuple, list)) else y).float()
        if E2E:                                                       # [B, 300, 6]  x1 y1 x2 y2 score cls  (already NMS-free)
            sc = y[..., 4] * torch.isin(y[..., 5].long(), self.keep).float()   # drop the classes Chira does not use, then re-sort
            sv, si = sc.topk(300, dim=1)
            return torch.cat([y[..., :4].gather(1, si.unsqueeze(-1).expand(-1, -1, 4)), sv.unsqueeze(-1), y[..., 5:6].gather(1, si.unsqueeze(-1))], -1)
        box, cls = y[:, :4].transpose(1, 2), y[:, 4:].transpose(1, 2)  # [B, 4+80, N], xywh in input pixels
        best, bi = cls[..., self.keep].max(-1)
        sv, si = best.topk(300, dim=1)
        b = box.gather(1, si.unsqueeze(-1).expand(-1, -1, 4))
        c = self.keep[bi.gather(1, si)].float()
        return torch.cat([b[..., :2] - b[..., 2:] / 2, b[..., :2] + b[..., 2:] / 2, sv.unsqueeze(-1), c.unsqueeze(-1)], -1)

w = Wrapper(det).cuda().eval()
x = torch.randint(0, 255, (1, *IMGSZ, 3), dtype=torch.uint8).cuda()
with torch.no_grad():
    print("out", w(x).shape)
    torch.onnx.export(w, x, out, input_names=["images"], output_names=["output"], opset_version=17, dynamic_axes={"images": {0: "batch"}, "output": {0: "batch"}})
print("ok")
