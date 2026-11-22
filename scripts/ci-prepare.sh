#!/usr/bin/env bash
# Make a clean clone buildable in CI.
#  * The AutoMiner (pow/miner) and the bot SDK (pow/sdk, which wraps the miner) depend on the
#    research crates of the private Gumbo64/sm64-AI-binds repo; they are always dropped from the
#    workspace here (KEEP_MINER=1 keeps them); the bundle then has no sm64coin-automine.
#  * The website imports the shared replay player from Gumbo64/sm64-viewer and that imports
#    sm64-binds' JS package from a sibling checkout: fetch both next to this repo.
#    (Delete this part when the player ships inside the sm64-binds npm package.)
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ "${KEEP_MINER:-0}" != 1 ]; then
  sed -i -E 's/, *"(miner|sdk)"//g; s/"(miner|sdk)" *, *//g' "$ROOT/pow/Cargo.toml"
  grep -n '^members' "$ROOT/pow/Cargo.toml"
fi
if [ "${1:-}" = "--web" ]; then
  PARENT="$(dirname "$ROOT")"
  VIEWER_REF="${VIEWER_REF:-e1fcfcb17d517e38e3ea77d32841b20934983c46}"
  BINDS_REF="${BINDS_REF:-v2.1.1}"
  [ -d "$PARENT/sm64-binds" ] || git clone -q https://github.com/Gumbo64/sm64-binds "$PARENT/sm64-binds" && git -C "$PARENT/sm64-binds" checkout -q "$BINDS_REF"
  [ -d "$PARENT/sm64-viewer" ] || git clone -q https://github.com/Gumbo64/sm64-viewer "$PARENT/sm64-viewer"
  git -C "$PARENT/sm64-viewer" checkout -q "$VIEWER_REF"
  (cd "$PARENT/sm64-viewer/frontend" && npm ci --ignore-scripts --no-audit --no-fund)
fi
