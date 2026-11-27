#!/usr/bin/env bash
# Build the light-client WASM (pow/lc-wasm: node list, failover, HTTPS + iroh-over-relay, header verification) into
# web/src/wasm/lc (git-ignored). Called by scripts/build-wasm.sh; also usable on its own.
# Needs: rustup target add wasm32-unknown-unknown, wasm-pack (npm i -g wasm-pack@0.13.1), clang (wasm32 C code: ring).
# The crate has its own Cargo.lock (wasm-bindgen 0.2.108: see pow/lc-wasm/Cargo.toml), so it never touches the web miner's pin.
# The wasm is large (~1.4 MB gzipped: iroh + rustls + the consensus rules); the site loads it lazily (web/src/lib/lightnet.js).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CC_wasm32_unknown_unknown="${CC_wasm32_unknown_unknown:-$(command -v clang || echo /usr/lib/llvm-18/bin/clang)}"
export AR_wasm32_unknown_unknown="${AR_wasm32_unknown_unknown:-$(command -v llvm-ar || echo /usr/lib/llvm-18/bin/llvm-ar)}"
# getrandom 0.3 needs its browser backend selected by cfg
export RUSTFLAGS="${RUSTFLAGS:-} --cfg getrandom_backend=\"wasm_js\""
# separate target dir: the other wasm crates must not see this RUSTFLAGS
export CARGO_TARGET_DIR="${LC_WASM_TARGET_DIR:-$ROOT/pow/lc-wasm/target}"
wasm-pack build "$ROOT/pow/lc-wasm" --release --target web --no-opt --out-dir "$ROOT/web/src/wasm/lc" --out-name sm64coin_lc_wasm
