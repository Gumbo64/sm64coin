#!/usr/bin/env bash
# Interactive wallet CLI against the local daemon. Usage: wallet-cli.sh [extra args]
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"
mkdir -p "$WALLET_DIR"
exec "$BIN/sm64coin-wallet-cli" --daemon-address "${DAEMON_URL#http://}" "$@"
