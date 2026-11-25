#!/usr/bin/env bash
# run.sh [VECTORS.jsonl]: the proof_id port against native Rust ids, natively (g++) and compiled to wasm (emcc, run in node).
# Make vectors: python3 make-vectors.py > in.jsonl; (cd pow && cargo run --release -p sm64pow --example proofid_vectors_v04 in.jsonl > vectors-v04.jsonl)
# WASM=0: the native run only (CI: wallet-engine.yml). Needs the fork's src/crypto/keccak.c ($MONERO_SRC, default ../../../monero/src), boost headers for it ($BOOST_INC for the wasm run, e.g. monero-ts/build/boost/include) and emsdk.
set -euo pipefail
D="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; W="$D/.."
KECCAK="${MONERO_SRC:-$D/../../../monero/src}/crypto"
V="${1:-$D/vectors-v04.jsonl}"
OUT="$(mktemp -d)"; trap 'rm -rf "$OUT"' EXIT
SRC=("$D/proofid_test.cpp" "$W/sm64coin_wasm.cpp")
CSRC=("$KECCAK/keccak.c")
INC=(-I"$D/stub" -I"$KECCAK/../../contrib/epee/include" -I"$KECCAK")
for c in "${CSRC[@]}"; do gcc -O1 -c "${INC[@]}" "$c" -o "$OUT/$(basename "$c").o"; done
g++ -std=c++17 -O1 -fsanitize=address,undefined "${INC[@]}" "${SRC[@]}" "$OUT"/*.o -o "$OUT/native" 2>&1 | grep -v "^$" || true
echo "== native (asan/ubsan)"; "$OUT/native" "$V"
if [ "${WASM:-1}" != 0 ] && { [ -n "${EMSDK:-}" ] || [ -f /usr/local/emsdk/emsdk_env.sh ]; }; then
  [ -n "${EMSDK:-}" ] || source /usr/local/emsdk/emsdk_env.sh >/dev/null 2>&1
  for c in "${CSRC[@]}"; do emcc -Oz -c "${INC[@]}" ${BOOST_INC:+-I"$BOOST_INC"} "$c" -o "$OUT/w_$(basename "$c").o"; done
  em++ -std=c++17 -Oz "${INC[@]}" "${SRC[@]}" "$OUT"/w_*.o -s NODERAWFS=1 -s ALLOW_MEMORY_GROWTH=1 -o "$OUT/wasm.js"
  echo "== wasm (node)"; node "$OUT/wasm.js" "$V"
fi
