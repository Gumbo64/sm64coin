#!/usr/bin/env bash
# The hard-star policy: a course gets two short search budgets and two checkpoints, each tree a 45 s wall-clock cap, and
# a cap-switch course is never chosen (a switch is a valid block but leaves the next block inside the secret course,
# where the pause-menu exit is unreliable). The campaign then moves to the next course instead of committing to one
# hard star. Use the usual automine flags after the script name; they override these defaults.
#   SM64COIN_AUTOMINE=/path/to/automine scripts/automine-fast.sh --address SM64... --daemon http://127.0.0.1:16481
set -euo pipefail
BIN="${SM64COIN_AUTOMINE:-}"
if [ -z "$BIN" ]; then
  if command -v sm64coin-automine >/dev/null 2>&1; then BIN=sm64coin-automine; else BIN=automine; fi
fi
exec "$BIN" --rrt-course-frames 20000000 --rrt-checkpoints 2 --rrt-step-seconds 45 --no-caps "$@"
