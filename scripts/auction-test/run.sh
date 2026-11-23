#!/usr/bin/env bash
# The frame-length auction on real nodes with real runs (see README.md here).
#   scripts/auction-test/run.sh CHAIN_DATA WORK_DIR
# CHAIN_DATA: a node data dir whose tip is FRESH (a block found within the last ~20 min: the
#   limit must still be growing for runs to wait for it). It is copied, never modified.
# WORK_DIR: scratch (node copies, saved runs, logs). Uses ports 17580-17582 and 17680-17682.
# Steps: mine three runs on the tip without submitting them (miner 1 twice from the same
# template, miner 2 once), then:
#   test 1  one miner, both runs held: the shorter replaces the longer (sent first)
#   test 2  two miners: the shorter run, sent later, becomes the block; node B follows over p2p
# Runs are submitted with the website's own code (web/src/lib/rules.js, proof.js).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"; . scripts/env.sh >/dev/null
BASE="$(cd "$1" && pwd)"; W="$(mkdir -p "$2" && cd "$2" && pwd)"
AM="$ROOT/pow/target/release/automine"
[ -x "$AM" ] || (cd pow && cargo build --release -p sm64coin-miner --bin automine --bin rrtrun)
(cd web && npx esbuild "$ROOT/scripts/auction-test/websubmit.mjs" --bundle --platform=node --format=esm \
   --outfile="$W/websubmit.mjs" --log-level=warning --alias:sm64-binds-frontend="$ROOT/scripts/auction-test/stub.mjs")
M1=SM64ps2jTVF19Pe2hfmCA3fxeAKYHJYyqPZpk351uXJdXaeXyiqju134HQUxhvXwQkRdS2uyj73jdCfgELsQe6HQ3QEoe7TqVn
M2=SM64RtPqsHM63LZn2u9Myx9HtC4kjhu1h98SRtJz9USB7NZAoCMdZSBD7D1L2DWu9uCYZFUSVKytreuzEMJ6VjBU2tbjKqevFH

node_pid() { for p in $(pgrep -f "$1"); do readlink /proc/$p/exe | grep -q monerod && echo $p; done | head -1; }
stop() { local f=$W/$1-run/node.pid; [ -f "$f" ] || return 0; local p; p=$(cat "$f"); kill "$p" 2>/dev/null || true; while kill -0 "$p" 2>/dev/null; do sleep 1; done; rm -f "$f"; }
start() { # name p2p rpc zmq difficulty extra
  stop "$1"; rm -rf "$W/$1" "$W/$1-run"; cp -r "$BASE" "$W/$1"; rm -f "$W/$1/p2pstate.bin"; mkdir -p "$W/$1-run"
  RUN_DIR=$W/$1-run SOLO=1 SEEDS_FILE=/dev/null NO_DHT=1 NODE_DATA=$W/$1 P2P_PORT=$2 RPC_PORT=$3 ZMQ_PORT=$4 \
    EXTRA_ARGS="--fixed-difficulty $5 --log-level 1 --allow-local-ip $6" scripts/start-node.sh >/dev/null
  until curl -s -m2 "http://127.0.0.1:$3/get_info" >/dev/null; do sleep 1; done
  node_pid "$W/$1 " > "$W/$1-run/node.pid"
}
frames() { python3 -c "import json; print(json.load(open('$1'))['frames'])"; }
tip() { curl -s "http://127.0.0.1:$1/json_rpc" -d '{"jsonrpc":"2.0","id":0,"method":"get_sm64_cycle"}' | python3 -c "
import json,sys; b=json.load(sys.stdin)['result']['blocks'][-1]; print('height', b['height'] + 1, 'tip frames', b['frames'], 'hash', b['hash'][:12])"; }
holdlog() { grep -E "holding block|fits sooner|held block|Proof-of-play rejected|invalid timestamp" "$W/$1-run/node-daemon.log" | cut -c1-200; }

echo "== mining three runs on the tip (not submitted)"
start mine 17580 17581 17582 16777216 ""
mine() { # name addr salt
  rm -rf "$W/camp-$1"
  RAYON_NUM_THREADS=4 "$AM" --address "$2" --daemon http://127.0.0.1:17581 --rom "$ROM" --rh-threads 4 \
    --rrt-scratch "$W/camp-$1" --salt "$3" --save-block "$W/$1.json" > "$W/$1.log" 2>&1
  echo "  $1: $(frames "$W/$1.json") frames"
}
mine r1 $M1 0 & mine r2 $M1 1000 & mine r3 $M2 0 &
wait
stop mine
echo "tip age: $(( $(date +%s) - $(python3 -c "import json; print(json.load(open('$W/r1.json'))['parent_ts'])") )) s"

pick() { "$AM" --address $M1 --daemon http://127.0.0.1:17581 --rom "$ROM" --pick-difficulty "$1" --hold-in "$2" 2>/dev/null; }
F1=$(frames "$W/r1.json"); F2=$(frames "$W/r2.json"); F3=$(frames "$W/r3.json")
if [ "$F1" -ge "$F2" ]; then L=r1; S=r2; else L=r2; S=r1; fi

echo "== test 1: one miner, same template, both held: the shorter replaces the longer"
D=$(pick "$W/$S.json" 70); echo "difficulty $D: the shorter run fits ~70 s from now"
start nodeA 17580 17581 17582 "$D" ""
node "$W/websubmit.mjs" "$W/$L.json" "$D" http://127.0.0.1:17581
sleep 3
node "$W/websubmit.mjs" "$W/$S.json" "$D" http://127.0.0.1:17581
sleep 100
holdlog nodeA; echo -n "node A: "; tip 17581
stop nodeA

echo "== test 2: two miners: the shorter run, sent later, becomes the block; node B follows"
if [ "$F3" -lt "$(frames "$W/$S.json")" ]; then L2=$S; S2=r3; else L2=r3; S2=$S; fi
D=$(pick "$W/$L2.json" 125); echo "difficulty $D: the longer run fits ~125 s from now"
start nodeA 17580 17581 17582 "$D" ""
start nodeB 17680 17681 17682 "$D" "--add-exclusive-node 127.0.0.1:17580"
sleep 10
node "$W/websubmit.mjs" "$W/$L2.json" "$D" http://127.0.0.1:17581
sleep 3
node "$W/websubmit.mjs" "$W/$S2.json" "$D" http://127.0.0.1:17581
sleep 150
holdlog nodeA; echo -n "node A: "; tip 17581; echo -n "node B: "; tip 17681
stop nodeA; stop nodeB
