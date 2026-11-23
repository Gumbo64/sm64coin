#!/usr/bin/env bash
# Make a clean clone buildable in CI.
#  * The AutoMiner (pow/miner) and the bot SDK (pow/sdk) build from a clean clone: the search
#    crates come from the public github.com/Gumbo64/sm64-AI-binds (a git dependency) and no ROM is
#    needed at compile time. DROP_MINER=1 drops them from the workspace (a quicker build).
#  * `--web` is accepted and does nothing: the website builds from npm alone (sm64-binds-frontend,
#    which ships the replay player), with no sibling checkouts.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ "${DROP_MINER:-0}" = 1 ]; then
  sed -i -E 's/, *"(miner|sdk)"//g; s/"(miner|sdk)" *, *//g' "$ROOT/pow/Cargo.toml"
  grep -n '^members' "$ROOT/pow/Cargo.toml"
fi
