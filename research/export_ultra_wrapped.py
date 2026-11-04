"""Ultralytics model -> ONNX with the SAME interface as the YOLOX export used by the Rust app:
   input  uint8 [B,544,960,3] RGB (dynamic batch)      output  fp32 [B,300,6]  x1 y1 x2 y2 score cls(COCO-80)
Layout/scale conversion, class filter (bicycle car motorcycle bus truck) and top-300 run on the GPU; NMS stays on the CPU.
usage: export_ultra_wrapped.py weights.pt out.onnx [e2e]"""
import sys, torch
from ultralytics import YOLO
pt, out = sys.argv[1], sys.argv[2]
det = YOLO(pt).model.fuse().eval()
E2E = len(sys.argv) > 3 and sys.argv[3] == "e2e"
head = det.model[-1]; head.export = True
head.end2end = E2E   # e2e: YOLO26 one2one head (trained NMS-free, ~300 clean boxes); otherwise one2many head + external NMS
for p in det.parameters(): p.requires_grad = False

class Wrapper(torch.nn.Module):
    def __init__(self, m):
        super().__init__(); self.m = m.half()
        self.register_buffer("keep", torch.tensor([1, 2, 3, 5, 7]))
    def forward(self, x):
        x = x.permute(0, 3, 1, 2).half() / 255                       # uint8 NHWC RGB -> fp16 NCHW 0..1
        y = self.m(x)
        y = (y[0] if isinstance(y, (tuple, list)) else y).float()
        if E2E:                                                       # [B, 300, 6]  x1 y1 x2 y2 score cls  (already NMS-free)
            sc = y[..., 4] * torch.isin(y[..., 5].long(), self.keep).float()   # drop non-traffic classes, then re-sort
            sv, si = sc.topk(300, dim=1)
            return torch.cat([y[..., :4].gather(1, si.unsqueeze(-1).expand(-1, -1, 4)), sv.unsqueeze(-1), y[..., 5:6].gather(1, si.unsqueeze(-1))], -1)
        box, cls = y[:, :4].transpose(1, 2), y[:, 4:].transpose(1, 2)  # [B, 4+80, N], xywh in input pixels
        best, bi = cls[..., self.keep].max(-1)
        sv, si = best.topk(300, dim=1)
        b = box.gather(1, si.unsqueeze(-1).expand(-1, -1, 4))
        c = self.keep[bi.gather(1, si)].float()
        return torch.cat([b[..., :2] - b[..., 2:] / 2, b[..., :2] + b[..., 2:] / 2, sv.unsqueeze(-1), c.unsqueeze(-1)], -1)

w = Wrapper(det).cuda().eval()
x = torch.randint(0, 255, (1, 544, 960, 3), dtype=torch.uint8).cuda()
with torch.no_grad():
    print("out", w(x).shape)
    torch.onnx.export(w, x, out, input_names=["images"], output_names=["output"], opset_version=17, dynamic_axes={"images": {0: "batch"}, "output": {0: "batch"}})
print("ok")
