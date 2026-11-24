#!/usr/bin/env bash
# Build the browser wallet engine (web/public/wallet/, git-ignored): the fork's wallet2 compiled to
# WebAssembly with monero-ts's build (woodser/monero-ts: wallet2 + boost + libsodium via emscripten),
# pointed at THIS repo's monero submodule so SM64Coin's constants and block format are used.
#   monero.worker.js     web worker with the wasm (8 MB)
#   monero-ts.lib.mjs    the main-thread API (0.7 MB)
# Needs: emsdk (EMSDK env, `source emsdk_env.sh`), cmake, node/npm, git, curl, gcc (openssl headers).
# First run takes ~30 min (boost, openssl, ~600 files); WORK is a cache. Uses at most JOBS (8) cores.
# SM64Coin-specific parts: web/wallet-wasm/sm64coin_wasm.cpp (the block id's proof_id, without the Rust
# game engine) and monero-ts-sm64.patch (c++17 for the newer emscripten, boost/clang fixes, the
# worker/lib webpack configs). See web/wallet-wasm/README.md.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HERE="$ROOT/web/wallet-wasm"
WORK="${WORK:-$ROOT/web/wallet-wasm/work}"
JOBS="${JOBS:-8}"
MONERO_TS_REV=919ee6453136ad7a7ceca3d77822436be93758cb
UNBOUND=1.22.0
[ -n "${EMSDK:-}" ] || { echo "run: source /path/to/emsdk/emsdk_env.sh" >&2; exit 1; }
[ -f "$ROOT/monero/CMakeLists.txt" ] || { echo "the monero submodule is empty: git submodule update --init monero" >&2; exit 1; }
# the fork's own header-only dependency submodule (wallet2's rapidjson); the rest of the fork's submodules aren't needed
[ -f "$ROOT/monero/external/rapidjson/include/rapidjson/document.h" ] || git -C "$ROOT/monero" submodule update --init --depth 1 external/rapidjson
mkdir -p "$WORK/shim"
printf '#!/bin/sh\necho %s\n' "$JOBS" > "$WORK/shim/nproc"; chmod +x "$WORK/shim/nproc"   # the upstream scripts use `nproc`
export PATH="$WORK/shim:$PATH"

cd "$WORK"
if [ ! -d monero-ts ]; then
  git -c core.autocrlf=false clone https://github.com/monero-ecosystem/monero-ts.git monero-ts
  git -C monero-ts config core.autocrlf false   # (a global core.autocrlf=true would CRLF the files and the patch would not apply)
  git -C monero-ts checkout "$MONERO_TS_REV"
  git -C monero-ts -c core.autocrlf=false submodule update --init external/monero-cpp
  (cd monero-ts && git -c core.autocrlf=false apply --whitespace=nowarn "$HERE/monero-ts-sm64.patch")
  ln -s "$HERE" monero-ts/sm64-wallet-wasm
  # monero-cpp's own monero-project (woodser's fork of upstream) is replaced by our fork
  rmdir monero-ts/external/monero-cpp/external/monero-project 2>/dev/null || true
  ln -sfn "$ROOT/monero" monero-ts/external/monero-cpp/external/monero-project
fi
# unbound's header (wallet2's DNS code includes it)
[ -d "unbound-$UNBOUND" ] || { curl -sL "https://nlnetlabs.nl/downloads/unbound/unbound-$UNBOUND.tar.gz" | tar xz; }
export UNBOUND_DIR="$WORK/unbound-$UNBOUND"
# translation_files.h (wallet2 includes it)
T="$ROOT/monero/build/release/translations"
if [ ! -f "$T/translation_files.h" ]; then
  mkdir -p "$T"; (cd "$T" && cmake -DCMAKE_BUILD_TYPE=Release "$ROOT/monero/translations" >/dev/null && make -j"$JOBS")
fi

cd monero-ts
[ -f build/openssl/include/openssl/ssl.h ] || sh bin/build_openssl_emscripten.sh
[ -f build/boost/lib/libboost_thread.a ] || sh bin/build_boost_emscripten.sh
# boost's is_signed/is_unsigned on enums with a fixed underlying type don't compile with current clang
sed -i 's/ || ::boost::is_enum<T>::value//' build/boost/include/boost/type_traits/is_signed.hpp build/boost/include/boost/type_traits/is_unsigned.hpp
[ -d node_modules ] || npm install --no-audit --no-fund
sh bin/build_wasm_emscripten.sh        # dist/monero.js (wasm inlined)
npm run build_web_worker               # dist/monero.worker.js
npx webpack --config webpack.lib.js    # dist/monero-ts.lib.mjs
OUT="$ROOT/web/public/wallet"
mkdir -p "$OUT"
cp dist/monero.worker.js dist/monero-ts.lib.mjs "$OUT/"
ls -la "$OUT"
