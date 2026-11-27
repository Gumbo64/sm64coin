#!/usr/bin/env bash
# Regenerates the light clients' compiled-in trust anchor (pow/lc/checkpoints/mainnet.json) from a node YOU trust (your own,
# fully synced): every block is fetched from genesis and run through the verifier's own header rules, then the genesis
# checkpoint plus one DEPTH blocks below the tip are written. Run it before tagging a release (packaging/RELEASING.md) and
# commit the file; the release workflow warns when the compiled checkpoint is far behind the public node's tip.
#
#   scripts/update-checkpoint.sh [node url] [depth]      defaults: http://127.0.0.1:16481, 36 (the node's reorg cap)
#   CHECK_ONLY=1 scripts/update-checkpoint.sh https://eboy.au     only check the committed table (on the chain, not stale)
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
node="${1:-http://127.0.0.1:16481}"
depth="${2:-36}"
cd "$here/pow/lc"
if [ "${CHECK_ONLY:-}" = 1 ]; then
  exec cargo run -q --release --example checkpoint --features http,tls,consensus -- "$node" --check checkpoints/mainnet.json --max-behind "${MAX_BEHIND:-2000}"
fi
cargo run -q --release --example checkpoint --features http,tls,consensus -- "$node" --depth "$depth" --out checkpoints/mainnet.json
# the table must still parse, validate and start at the genesis block the apps and the site know
cargo test -q --lib checkpoints
git -C "$here" diff --stat -- pow/lc/checkpoints/mainnet.json
echo "Commit pow/lc/checkpoints/mainnet.json with the release."
