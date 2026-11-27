#!/usr/bin/env bash
# Build the browser WASM modules the website imports (web/src/wasm/*; perturb is git-ignored).
# Needs: rustup target add wasm32-unknown-unknown, wasm-pack (npm i -g wasm-pack).
# The web miner also needs clang + llvm-ar (wasm32 C code) and access to the git dependencies in
# pow/webminer/Cargo.toml; set NO_WEBMINER=1 to skip it, REQUIRE_WEBMINER=1 to fail when it does not build.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build() { # crate-dir out-dir out-name
  wasm-pack build "$ROOT/pow/$1" --release --target web --no-opt --out-dir "$ROOT/web/src/wasm/$2" --out-name "$3"
}
build perturb-wasm perturb sm64coin_perturb_wasm
# The light-client network (pow/lc-wasm, own lockfile and build flags): the site's node access, loaded lazily. NO_LCWASM=1 skips it
# (then `vite build` needs web/src/wasm/lc from elsewhere: the site imports it).
if [ -z "${NO_LCWASM:-}" ]; then "$ROOT/scripts/build-lc-wasm.sh"; fi
# The web miner (the native miner's RRT campaign, pow/webminer): needs the search crates
# (objective-rrt-v2) and a C compiler for wasm32 (clang); skipped when they aren't there.
if [ -z "${NO_WEBMINER:-}" ]; then
  export CC_wasm32_unknown_unknown="${CC_wasm32_unknown_unknown:-$(command -v clang || echo /usr/lib/llvm-18/bin/clang)}"
  export AR_wasm32_unknown_unknown="${AR_wasm32_unknown_unknown:-$(command -v llvm-ar || echo /usr/lib/llvm-18/bin/llvm-ar)}"
  if [ -n "${REQUIRE_WEBMINER:-}" ]; then
    build webminer webminer sm64coin_webminer
  else
    build webminer webminer sm64coin_webminer || echo "build-wasm.sh: web miner not built (the site works without it)" >&2
  fi
fi
exit 0
