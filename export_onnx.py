"""usage: export_onnx.py out.onnx [fp16]   (run with a python that has torch + the repo deps)"""
import os, sys, torch
_repo = os.environ.get("YOLOX_REPO", "../airi_tracker")   # the old airi_tracker checkout (provides the YOLOX code)
sys.path.insert(0, _repo); sys.path.insert(0, _repo + "/services/yoloX")
from services.yoloX.yolox.exp import get_exp
from services.yoloX.yolox.utils import replace_module
from services.yoloX.yolox.models.network_blocks import SiLU
import os
exp = get_exp(None, os.environ.get("YOLOX_NAME", "yolox-s"))
model = exp.get_model()
ck = torch.load(os.environ.get("YOLOX_CKPT", "yolox_s.pth"), map_location="cpu")
model.load_state_dict(ck["model"]); model.eval()
model = replace_module(model, torch.nn.SiLU, SiLU)
model.head.decode_in_inference = False  # raw head output; we decode in fp32 below


TOPK = 300


class Wrapper(torch.nn.Module):
    """fp32 in/out, fp16 backbone+head, fp32 box decode (fp16 loses sub-pixel precision at x>512)."""

    def __init__(self, m, half):
        super().__init__()
        self.m, self.half = m, half
        grids, strides = [], []
        for s in (8, 16, 32):
            yv, xv = torch.meshgrid(torch.arange(544 // s), torch.arange(960 // s), indexing="ij")
            g = torch.stack((xv, yv), 2).view(1, -1, 2).float()
            grids.append(g)
            strides.append(torch.full((1, g.shape[1], 1), float(s)))
        self.register_buffer("keep", torch.tensor([1, 2, 3, 5, 7]))  # bicycle car motorcycle bus truck
        self.register_buffer("grids", torch.cat(grids, 1))
        self.register_buffer("strides", torch.cat(strides, 1))

    def forward(self, x):
        # x: uint8 [B,H,W,3] RGB straight from ffmpeg (dynamic batch). Layout/BGR/float conversion on the GPU.
        x = x.permute(0, 3, 1, 2).flip(1)
        x = x.half() if self.half else x.float()
        raw = self.m(x).float()                                   # [B, N, 85]
        xy = (raw[..., 0:2] + self.grids) * self.strides
        wh = torch.exp(raw[..., 2:4]) * self.strides
        # score = objectness * best class score, restricted to the traffic classes
        best, bi = raw[..., 5:][..., self.keep].max(-1)
        score = raw[..., 4] * best
        # keep only the top-K candidates on the GPU -> tiny [B, K, 6] result instead of [B, N, 85]
        sv, si = score.topk(TOPK, dim=1)
        idx = si.unsqueeze(-1)
        xy, wh = xy.gather(1, idx.expand(-1, -1, 2)), wh.gather(1, idx.expand(-1, -1, 2))
        cls = self.keep[bi.gather(1, si)].float()
        return torch.cat([xy - wh / 2, xy + wh / 2, sv.unsqueeze(-1), cls.unsqueeze(-1)], -1)  # x1 y1 x2 y2 score cls


half = len(sys.argv) > 2 and sys.argv[2] == "fp16"
dev = "cuda" if half else "cpu"
w = Wrapper(model.half() if half else model, half).to(dev).eval()
x = torch.randint(0, 255, (1, 544, 960, 3), dtype=torch.uint8).to(dev)
with torch.no_grad():
    torch.onnx.export(w, x, sys.argv[1], input_names=["images"], output_names=["output"], opset_version=17,
                      dynamic_axes={"images": {0: "batch"}, "output": {0: "batch"}})
print("ok")
