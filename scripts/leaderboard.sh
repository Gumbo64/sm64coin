#!/usr/bin/env bash
# Start the opt-in mining leaderboard in the background. Env: LB_BIND (127.0.0.1:16490), LB_DATA, DAEMON_URL
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"
LB_BIND="${LB_BIND:-127.0.0.1:16490}"
LB_DATA="${LB_DATA:-$ROOT/data/leaderboard}"
PIDF="$RUN_DIR/leaderboard.pid"
if [ -f "$PIDF" ] && kill -0 "$(cat "$PIDF")" 2>/dev/null; then echo "already running (pid $(cat "$PIDF"))"; exit 0; fi
(cd "$ROOT/pow" && cargo build --release -p sm64coin-leaderboard)
# Registrations hold private view keys: keep them readable by this user only.
mkdir -p "$LB_DATA" && chmod 700 "$LB_DATA"
umask 077
setsid "$ROOT/pow/target/release/sm64coin-leaderboard" --daemon "$DAEMON_URL" --bind "$LB_BIND" --data-dir "$LB_DATA" \
  >"$RUN_DIR/leaderboard.log" 2>&1 < /dev/null &
echo $! > "$PIDF"
echo "leaderboard pid $(cat "$PIDF") on http://$LB_BIND (log: run/leaderboard.log)"
