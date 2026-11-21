#!/usr/bin/env bash
# Start the SM64Coin daemon in the background.
# Env: SOLO=1 (first node of a new network), PEERS="host:port host:port" (added to the peers in seeds.txt; SEEDS_FILE overrides the file), NODE_DATA, ROM, P2P_PORT, RPC_PORT, ZMQ_PORT, RUN_DIR, EXTRA_ARGS
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"
[ -x "$BIN/sm64coind" ] || { echo "missing $BIN/sm64coind (run: make build)" >&2; exit 1; }
[ -f "$ROM" ] || { echo "missing ROM $ROM (US baserom.us.z64, sha1 9bef1128717f958171a4afac3ed78ee2bb4e86ce)" >&2; exit 1; }
PIDF="$RUN_DIR/node.pid"
if [ -f "$PIDF" ] && kill -0 "$(cat "$PIDF")" 2>/dev/null; then echo "node already running (pid $(cat "$PIDF"))"; exit 0; fi
mkdir -p "$NODE_DATA"
args=(--data-dir "$NODE_DATA" --sm64-rom "$ROM"
  --p2p-bind-port "$P2P_PORT" --rpc-bind-port "$RPC_PORT" --zmq-rpc-bind-port "$ZMQ_PORT"
  --non-interactive --log-file "$RUN_DIR/node-daemon.log")
[ "${SOLO:-0}" = 1 ] && args+=(--sm64-solo)
for p in $(seed_peers) ${PEERS:-}; do args+=(--add-peer "$p"); done
# shellcheck disable=SC2206
set -f  # no globbing: EXTRA_ARGS may contain *
args+=(${EXTRA_ARGS:-})
set +f
setsid "$BIN/sm64coind" "${args[@]}" </dev/null >"$RUN_DIR/node.log" 2>&1 &
echo $! >"$PIDF"
echo "node started, pid $(cat "$PIDF"), rpc $DAEMON_URL, log $RUN_DIR/node.log"
