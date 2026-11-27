#!/usr/bin/env bash
# Start the SM64Coin daemon in the background.
# Env: SOLO=1 (first node of a new network), GATEWAY="host:16480" (talk only to your own gateway; see docs/gateway.md), NO_DHT=1 (no Mainline DHT lookup), PEERS="host:port host:port" (added to the peers in seeds.txt; SEEDS_FILE overrides the file), NODE_DATA, ROM, P2P_PORT, RPC_PORT, ZMQ_PORT, RUN_DIR, EXTRA_ARGS
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
if [ -n "${GATEWAY:-}" ]; then
  # Only your own gateway (scripts/gateway-node.sh): no other peer sees this machine's IP, and
  # nothing connects in (no port to forward).
  for g in $GATEWAY; do args+=(--add-exclusive-node "$g"); done
  args+=(--hide-my-port --no-igd)
else
  for p in $(seed_peers) ${PEERS:-}; do args+=(--add-peer "$p"); done
  # Nodes announced in BitTorrent's Mainline DHT (the launcher's dht.rs); NO_DHT=1 skips it.
  LAUNCHER="$ROOT/pow/launcher/target/release/sm64coin"
  if [ "${NO_DHT:-0}" != 1 ] && [ -x "$LAUNCHER" ]; then
    for p in $(timeout 40 "$LAUNCHER" --dht-lookup 2>/dev/null | grep -E '^[0-9.]+:[0-9]+$' | head -24); do args+=(--add-peer "$p"); done
  fi
fi
# shellcheck disable=SC2206
set -f  # no globbing: EXTRA_ARGS may contain *
# The website on GitHub Pages calls the node from the visitor's browser (SITE_ORIGIN; RPC stays local).
case " ${EXTRA_ARGS:-} " in *--rpc-access-control-origins*) ;; *) args+=(--rpc-access-control-origins "${SITE_ORIGIN:-https://gumbo64.github.io,https://sm64coin.pages.dev,http://tauri.localhost}") ;; esac
# Monero allows one inbound p2p connection per IP; homes and carrier NAT share one (a flag, not a code change).
case " ${EXTRA_ARGS:-} " in *--max-connections-per-ip*) ;; *) args+=(--max-connections-per-ip=16) ;; esac
args+=(${EXTRA_ARGS:-})
set +f
setsid "$BIN/sm64coind" "${args[@]}" </dev/null >"$RUN_DIR/node.log" 2>&1 &
echo $! >"$PIDF"
( update_notice 2>/dev/null || true ) &   # at most 3 s, in the background
echo "node started, pid $(cat "$PIDF"), rpc $DAEMON_URL, log $RUN_DIR/node.log"
