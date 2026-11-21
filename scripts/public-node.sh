#!/usr/bin/env bash
# Run a node that others (and browsers) can use: restricted RPC on 0.0.0.0 with CORS, p2p open.
# Restricted RPC exposes read-only/public calls only (get_info, get_block, get_block_template, submit_block...,
# no stop/pop_blocks/set_*). Browsers need --rpc-access-control-origins; set ORIGINS to restrict it.
# Open firewall ports P2P_PORT (default 16480) and RPC_PORT (default 16481).
# Env: ORIGINS (default '*'), PEERS, SOLO=1 for the very first node, plus start-node.sh variables.
set -euo pipefail
export EXTRA_ARGS="--rpc-bind-ip 0.0.0.0 --restricted-rpc --confirm-external-bind --rpc-access-control-origins ${ORIGINS:-*} ${EXTRA_ARGS:-}"
exec "$(dirname "${BASH_SOURCE[0]}")/start-node.sh"
