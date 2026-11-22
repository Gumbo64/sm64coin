#!/usr/bin/env bash
# Build the browser WASM modules the website imports (web/src/wasm/*; perturb is git-ignored).
# Needs: rustup target add wasm32-unknown-unknown, wasm-pack (npm i -g wasm-pack).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build() { # crate-dir out-dir out-name
  wasm-pack build "$ROOT/pow/$1" --release --target web --no-opt --out-dir "$ROOT/web/src/wasm/$2" --out-name "$3"
}
build perturb-wasm perturb sm64coin_perturb_wasm
exit 0
