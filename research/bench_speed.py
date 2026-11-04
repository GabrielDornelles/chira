"""GPU latency of each exported detector (ONNX Runtime CUDA EP, 544x960, fp16). usage: bench_speed.py WORKDIR [providers]"""
import sys, time, glob, os, numpy as np, onnxruntime as ort
work = sys.argv[1]
only = sys.argv[3].split(",") if len(sys.argv) > 3 else None
if len(sys.argv) > 2 and sys.argv[2] == "trt":
    prov = [("TensorrtExecutionProvider", {"trt_fp16_enable": True, "trt_engine_cache_enable": True, "trt_engine_cache_path": work + "/trt_cache", "trt_max_workspace_size": 4 << 30}), ("CUDAExecutionProvider", {})]
else:
    prov = [("CUDAExecutionProvider", {})]
T = {"tensor(float)": np.float32, "tensor(float16)": np.float16, "tensor(uint8)": np.uint8}
for f in sorted(glob.glob(work + "/onnx/*.onnx")):
    name = os.path.basename(f)[:-5]
    if only and name not in only: continue
    try:
        so = ort.SessionOptions(); so.log_severity_level = 3
        s = ort.InferenceSession(f, so, providers=prov)
        i = s.get_inputs()[0]; o = s.get_outputs()
        res = []
        for b in (1, 4):
            shape = [b if k == 0 else {2: 544, 3: 960}.get(k, d) if isinstance(d, str) else d for k, d in enumerate(i.shape)]
            if len(shape) == 4 and shape[1] != 3 and shape[3] == 3: pass
            x = (np.random.rand(*shape) * 255).astype(T[i.type]) if i.type == "tensor(uint8)" else np.random.rand(*shape).astype(T[i.type])
            for _ in range(5): s.run(None, {i.name: x})
            t = time.perf_counter(); n = 30
            for _ in range(n): s.run(None, {i.name: x})
            res.append((time.perf_counter() - t) / n * 1000)
        print(f"{name:12s} in {i.type[7:-1]:7s}{str(i.shape):28s} out {[ (x.name, x.shape) for x in o][:2]}  b1 {res[0]:6.1f} ms  b4 {res[1]/4:6.1f} ms/frame", flush=True)
    except Exception as e:
        print(f"{name:12s} FAILED {str(e)[:150]}", flush=True)
