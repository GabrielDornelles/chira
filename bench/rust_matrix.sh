#!/usr/bin/env bash
# Rust benchmark matrix. usage: bench/rust_matrix.sh FPS "BATCH_CONFIGS" "CAMERA_COUNTS"
#   e.g. bench/rust_matrix.sh 30 "1 8" "2 3 5 6 8"        (BATCH_CONFIG = MAX_BATCH[:BATCH_WAIT_MS])
cd "$(dirname "$0")/.."
FPS=${1:-30}; CONFIGS=${2:-"1 8"}; COUNTS=${3:-"2 3 5 6 8"}
cargo build --release 2>&1 | tail -1
export LD_LIBRARY_PATH="$PWD/target/release:/usr/local/cuda/lib64:${LD_LIBRARY_PATH:-}"
add(){ curl -s -XPOST localhost:8787/api/cameras -H 'content-type: application/json' -d "{\"setup\":$1,\"fps\":$2}" >/dev/null; }
delall(){ for i in $(curl -s localhost:8787/api/stats | python3 -c "import json,sys;print(*[c['id'] for c in json.load(sys.stdin)['cameras']])"); do curl -s -XDELETE localhost:8787/api/cameras/$i >/dev/null; done; }
measure(){ python3 - "$1" <<'PY'
import json,sys,time,urllib.request
def get(): return json.load(urllib.request.urlopen("http://localhost:8787/api/stats"))
a=get(); t0=time.time(); g=[]
while time.time()-t0<float(sys.argv[1]):
    time.sleep(2); g.append(get()['gpu']['util'])
b=get(); dt=time.time()-t0
A={c['id']:c for c in a['cameras']}
rows=[]; tot=0
for c in b['cameras']:
    p=A[c['id']]; proc=(c['processed']-p['processed'])/dt; dr=(c['dropped']-p['dropped'])/dt
    rows.append((proc, 100*dr/max(1e-9,proc+dr), c['lat_ms'])); tot+=proc
n=len(rows)
print(f"  {n} cams: per-cam processed min {min(r[0] for r in rows):5.1f} avg {sum(r[0] for r in rows)/n:5.1f} fps | dropped avg {sum(r[1] for r in rows)/n:4.1f}% max {max(r[1] for r in rows):4.1f}% | latency avg {sum(r[2] for r in rows)/n:3.0f} ms | total {tot:5.0f} fps | GPU {sum(g)/len(g):.0f}% | batch avg {b['batch']['avg']:.1f} ({b['batch']['ms']:.1f} ms/call) | cpu {b['cpu_cores']:.1f} cores",flush=True)
PY
}
for CFG in $CONFIGS; do
  MB=${CFG%%:*}; WAIT=0; [[ $CFG == *:* ]] && WAIT=${CFG#*:}
  pkill -x chira; pkill ffmpeg; sleep 1
  MAX_BATCH=$MB BATCH_WAIT_MS=$WAIT ./target/release/chira > /tmp/rust_matrix_server.log 2>&1 &
  until curl -s localhost:8787/api/stats >/dev/null; do sleep 1; done
  echo "== MAX_BATCH=$MB BATCH_WAIT_MS=$WAIT, ${FPS} fps sources"
  for n in $COUNTS; do
    delall; sleep 2
    for i in $(seq $n); do add $((i%2+1)) $FPS; done
    sleep 12; measure 20
  done
  delall
done
pkill -x chira; pkill ffmpeg
echo DONE
