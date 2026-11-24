# Shared settings, sourced by the other scripts. Every value can be overridden from the environment.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${BIN:-$ROOT/bin}" # sm64coin-named links (make links) to the built binaries
ROM="${ROM:-$ROOT/baserom.us.z64}"
RUN_DIR="${RUN_DIR:-$ROOT/run}"
NODE_DATA="${NODE_DATA:-$ROOT/data/node}"
WALLET_DIR="${WALLET_DIR:-$ROOT/data/wallets}"
P2P_PORT="${P2P_PORT:-16480}"
RPC_PORT="${RPC_PORT:-16481}"
ZMQ_PORT="${ZMQ_PORT:-16482}"
WALLET_PORT="${WALLET_PORT:-16483}"
WEB_PORT="${WEB_PORT:-5181}"
SEEDS_FILE="${SEEDS_FILE:-$ROOT/seeds.txt}"
DAEMON_URL="${DAEMON_URL:-http://127.0.0.1:$RPC_PORT}"
WALLET_URL="${WALLET_URL:-http://127.0.0.1:$WALLET_PORT}"
mkdir -p "$RUN_DIR"

# stop_pidfile <file>: SIGTERM the pid in the file and wait; no pattern matching.
stop_pidfile() {
  local f="$1" pid
  [ -f "$f" ] || return 0
  pid="$(cat "$f")"
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    echo "stopping $(basename "$f" .pid) (pid $pid)"
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 60); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
    kill -0 "$pid" 2>/dev/null && { echo "  still alive, SIGKILL"; kill -9 "$pid" 2>/dev/null || true; }
  fi
  rm -f "$f"
}

# seed_peers: print host:port from $SEEDS_FILE (comments after # and blank lines ignored).
# Hostnames are resolved here and skipped when they don't resolve: the daemon refuses to start
# on an --add-peer it can't resolve.
seed_peers() {
  [ -f "$SEEDS_FILE" ] || return 0
  local e h port ip
  for e in $(sed -e 's/#.*//' -e 's/[[:space:]]//g' "$SEEDS_FILE" | grep -v '^$'); do
    h=${e%:*}; port=${e##*:}; [ "$h" = "$e" ] && port=16480
    if [[ "$h" =~ ^[0-9.]+$ ]]; then echo "$h:$port"; continue; fi
    ip=$(getent ahostsv4 "$h" 2>/dev/null | awk 'NR==1 {print $1}')
    if [ -n "$ip" ]; then echo "$ip:$port"; else echo "seed $h doesn't resolve: skipped" >&2; fi
  done
}

# update_notice: one log line if a newer SM64Coin release exists (GitHub API, cached 6 h, 3 s
# timeout, silent on any failure). The running version: $SM64COIN_VERSION or the bundle's VERSION
# file; neither (a checkout) or SM64COIN_NO_UPDATE_CHECK=1 -> no check. A release whose body has a
# line `Update: required` is flagged as such.
update_notice() {
  [ "${SM64COIN_NO_UPDATE_CHECK:-0}" = 1 ] && return 0
  local cur="${SM64COIN_VERSION:-}" cache="$RUN_DIR/latest-release.json" json tag latest
  [ -z "$cur" ] && [ -f "$ROOT/VERSION" ] && cur="$(head -n1 "$ROOT/VERSION")"
  cur="${cur#v}"
  case "$cur" in [0-9]*.[0-9]*.[0-9]*) ;; *) return 0 ;; esac
  if [ -z "$(find "$cache" -mmin -360 2>/dev/null)" ]; then
    command -v curl >/dev/null 2>&1 || return 0
    curl -fsS -m 3 -H 'Accept: application/vnd.github+json' "${SM64COIN_UPDATE_URL:-https://api.github.com/repos/Gumbo64/sm64coin/releases/latest}" -o "$cache.tmp" 2>/dev/null && mv -f "$cache.tmp" "$cache" || { rm -f "$cache.tmp"; return 0; }
  fi
  json="$(cat "$cache" 2>/dev/null)" || return 0
  tag="$(printf '%s' "$json" | sed -n 's/.*"tag_name" *: *"v\{0,1\}\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p' | head -n1)"
  [ -n "$tag" ] || return 0
  latest="$(printf '%s\n%s\n' "$cur" "$tag" | sort -V | tail -n1)"
  if [ "$latest" = "$tag" ] && [ "$tag" != "$cur" ]; then
    if printf '%s' "$json" | grep -Eiq '("|\\n)Update: *required(\\r)?(\\n|")'; then
      echo "SM64Coin $tag is available (UPDATE REQUIRED; you run $cur): https://github.com/Gumbo64/sm64coin/releases/tag/v$tag"
    else
      echo "SM64Coin $tag is available: https://github.com/Gumbo64/sm64coin/releases/tag/v$tag"
    fi
  fi
}
