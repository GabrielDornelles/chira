"""Widen the class filter baked into an exported Chira ONNX model (no re-export, no weights needed).
   usage: widen_classes.py in.onnx out.onnx            keeps the 15 classes listed in src/config.rs"""
import sys, numpy as np, onnx
from onnx import numpy_helper as nh
IDS = [0, 2, 7, 5, 3, 1, 6, 8, 16, 15, 14, 17, 24, 26, 28]   # person car truck bus motorcycle bicycle train boat dog cat bird horse backpack handbag suitcase
m = onnx.load(sys.argv[1])
hit = [i for i in m.graph.initializer if i.name == "keep"]
assert len(hit) == 1, "no 'keep' initializer: is this a Chira export?"
hit[0].CopyFrom(nh.from_array(np.array(IDS, dtype=np.int64), "keep"))
onnx.save(m, sys.argv[2])
print("ok:", sys.argv[2])
