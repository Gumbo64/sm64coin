#!/usr/bin/env bash
# Join an existing SM64Coin network: start a node that syncs from the given peers.
# Usage: scripts/join.sh <host:port> [<host:port> ...]    (peers in seeds.txt are used too)
# Env: same as start-node.sh (NODE_DATA, P2P_PORT, RPC_PORT, ...). No --sm64-solo: the node waits for peers.
set -euo pipefail
[ $# -ge 1 ] || { echo "usage: $0 <host:port> [<host:port> ...]" >&2; exit 2; }
export PEERS="$* ${PEERS:-}"
export SOLO=0
exec "$(dirname "${BASH_SOURCE[0]}")/start-node.sh"
