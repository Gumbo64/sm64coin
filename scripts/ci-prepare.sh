#!/usr/bin/env bash
# Make a clean clone buildable in CI.
#  * The AutoMiner (pow/miner) and the bot SDK (pow/sdk, which wraps the miner) depend on the
#    research crates of the private Gumbo64/sm64-AI-binds repo; they are always dropped from the
#    workspace here (KEEP_MINER=1 keeps them); the bundle then has no sm64coin-automine.
#  * `--web` is accepted and does nothing: the website builds from npm alone (sm64-binds-frontend,
#    which ships the replay player), with no sibling checkouts.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ "${KEEP_MINER:-0}" != 1 ]; then
  sed -i -E 's/, *"(miner|sdk)"//g; s/"(miner|sdk)" *, *//g' "$ROOT/pow/Cargo.toml"
  grep -n '^members' "$ROOT/pow/Cargo.toml"
fi
