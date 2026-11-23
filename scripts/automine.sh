#!/usr/bin/env bash
# Automated miner. Usage: automine.sh [SM64... address] [extra automine flags]
# Without an address, uses the first address of the open wallet via wallet RPC get_address.
# Env: ROUTE, DAEMON_URL, WALLET_URL, ROM
set -euo pipefail
# Simulation threads (THREADS, default 4).
export RAYON_NUM_THREADS="${THREADS:-4}"
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"
BINARY="$ROOT/pow/target/release/automine"
# Space-separated; tried in order (the first is the calmest to follow).
# Routes from files; the chain's own winning runs are added automatically.
ROUTE="${ROUTE:-$ROOT/pow/miner/routes/block1-human.m64}"
# Always (re)build: a stale miner with other rules would waste its runs.
(cd "$ROOT/pow" && cargo build --release -q -p sm64coin-miner --bin automine --bin rrtrun) # rrtrun: the tree search child automine spawns
[ -f "$ROM" ] || { echo "missing ROM $ROM" >&2; exit 1; }
if [ $# -ge 1 ] && [[ "$1" == SM64* ]]; then
  ADDR="$1"; shift
else
  ADDR="$(curl -s "$WALLET_URL/json_rpc" -d '{"jsonrpc":"2.0","id":0,"method":"get_address","params":{"account_index":0}}' \
    | sed -n 's/.*"address": *"\(SM64[^"]*\)".*/\1/p' | head -n1)"
  [ -n "$ADDR" ] || { echo "no address given and wallet RPC at $WALLET_URL has no open wallet (create/open one, or pass an address)" >&2; exit 1; }
fi
echo "mining to $ADDR via $DAEMON_URL"
ROUTE_ARGS=()
for r in $ROUTE; do ROUTE_ARGS+=(--route "$r"); done
exec "$BINARY" --address "$ADDR" --daemon "$DAEMON_URL" --rom "$ROM" "${ROUTE_ARGS[@]}" "$@"
