#!/usr/bin/env bash
# Web UI dev server (foreground, all interfaces). Env: WEB_PORT (5181), DAEMON_URL, WALLET_URL
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"
cd "$ROOT/web"
[ -d node_modules ] || npm install
export SM64COIN_DAEMON="$DAEMON_URL" SM64COIN_WALLET="$WALLET_URL"
exec npm run dev -- --host --port "$WEB_PORT"
