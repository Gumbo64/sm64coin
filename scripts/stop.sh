#!/usr/bin/env bash
# Stop node and wallet RPC started by these scripts (pid files only).
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"
stop_pidfile "$RUN_DIR/wallet-rpc.pid"
stop_pidfile "$RUN_DIR/node.pid"
