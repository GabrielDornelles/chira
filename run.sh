#!/usr/bin/env bash
# Builds and runs the multi-camera tracker. Defaults: YOLO26-M on TensorRT (FP16), max batch 4.
#   EP=cuda          use the plain CUDA provider (no TensorRT needed; ~2x slower)
#   MODEL=...onnx    other model (models/yolo26s_544x960.onnx, models/yolox_s_544x960_fp16.onnx ...)
#   MAX_BATCH=n      cameras batched per GPU call (TensorRT builds one engine per batch size on first start)
#   PORT=8787
set -e
cd "$(dirname "$0")"
cargo build --release
export EP="${EP:-trt}"
export MODEL="${MODEL:-models/yolo26m_544x960.onnx}"
TRT_LIBS="${TRT_LIBS:-$(ls -d "$PWD"/.trt-venv/lib/python3*/site-packages/tensorrt_libs 2>/dev/null | head -1)}"
export LD_LIBRARY_PATH="$PWD/target/release:/usr/local/cuda/lib64:${TRT_LIBS:+$TRT_LIBS:}${LD_LIBRARY_PATH:-}"
if [ "$EP" = trt ] && [ -z "$TRT_LIBS" ]; then
  echo "TensorRT libs not found. Run:  uv venv .trt-venv && VIRTUAL_ENV=.trt-venv uv pip install tensorrt-cu12-libs==10.9.0.34   (or EP=cuda ./run.sh)"; exit 1
fi
exec ./target/release/chira "$@"
