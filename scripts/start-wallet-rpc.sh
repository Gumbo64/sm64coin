#!/usr/bin/env bash
# Start sm64coin-wallet-rpc on 127.0.0.1 (no login; local use only).
# Env: WALLET_DIR, WALLET_PORT, DAEMON_URL, RUN_DIR
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"
[ -x "$BIN/sm64coin-wallet-rpc" ] || { echo "missing wallet rpc binary (run: make build)" >&2; exit 1; }
PIDF="$RUN_DIR/wallet-rpc.pid"
if [ -f "$PIDF" ] && kill -0 "$(cat "$PIDF")" 2>/dev/null; then echo "wallet rpc already running (pid $(cat "$PIDF"))"; exit 0; fi
mkdir -p "$WALLET_DIR"
setsid "$BIN/sm64coin-wallet-rpc" --wallet-dir "$WALLET_DIR" --rpc-bind-ip 127.0.0.1 --rpc-bind-port "$WALLET_PORT" \
  --disable-rpc-login --daemon-address "${DAEMON_URL#http://}" --non-interactive \
  --log-file "$RUN_DIR/wallet-rpc-daemon.log" </dev/null >"$RUN_DIR/wallet-rpc.log" 2>&1 &
echo $! >"$PIDF"
echo "wallet rpc started, pid $(cat "$PIDF"), $WALLET_URL, wallets in $WALLET_DIR"
