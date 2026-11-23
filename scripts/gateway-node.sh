#!/usr/bin/env bash
# A permanent gateway node on a server (a VPS): the network's seed, p2p only.
# Port 16480 is open to the world; RPC stays on 127.0.0.1 (restricted); no wallet, no miner.
# Run it from a clone on the server, as the user the node should run as:
#   git clone --recursive https://github.com/Gumbo64/sm64coin.git && cd sm64coin
#   cp /path/to/baserom.us.z64 .          # your own ROM: every node replays every block
#   scripts/gateway-node.sh               # packages, build, systemd unit, firewall
# Then point your own node at it (scripts/start-node.sh with GATEWAY=<this server>:16480) and add
# <this server>:16480 to seeds.txt. See docs/gateway.md.
# Env: JOBS (build jobs, default: cores), SKIP_PACKAGES=1, SKIP_BUILD=1, P2P_PORT (16480).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
P2P_PORT="${P2P_PORT:-16480}"
JOBS="${JOBS:-$(nproc)}"
[ -f baserom.us.z64 ] || { echo "copy your baserom.us.z64 into $ROOT first" >&2; exit 1; }

if [ "${SKIP_PACKAGES:-0}" != 1 ]; then
  sudo apt-get update
  sudo apt-get install -y git build-essential cmake pkg-config libssl-dev libzmq3-dev \
    libunbound-dev libsodium-dev libunwind8-dev liblzma-dev libreadline-dev libexpat1-dev \
    libpgm-dev libhidapi-dev libusb-1.0-0-dev libprotobuf-dev protobuf-compiler libudev-dev \
    libboost-all-dev python3 ccache curl
  command -v cargo >/dev/null || { curl https://sh.rustup.rs -sSf | sh -s -- -y; . "$HOME/.cargo/env"; }
fi
. "$HOME/.cargo/env" 2>/dev/null || true

if [ "${SKIP_BUILD:-0}" != 1 ]; then
  # A small server: a swap file keeps the C++ build from running out of memory.
  if [ "$(free -m | awk '/Mem:/{print $2}')" -lt 6000 ] && ! swapon --show | grep -q .; then
    sudo fallocate -l 4G /swapfile && sudo chmod 600 /swapfile && sudo mkswap /swapfile && sudo swapon /swapfile
  fi
  make build-monero JOBS="$JOBS"
  make links
fi

DATA="$ROOT/data/gateway"
mkdir -p "$DATA"
sudo tee /etc/systemd/system/sm64coin-gateway.service >/dev/null <<UNIT
[Unit]
Description=SM64Coin gateway node (p2p only)
After=network-online.target
Wants=network-online.target

[Service]
User=$USER
WorkingDirectory=$ROOT
ExecStart=$ROOT/bin/sm64coind --data-dir $DATA --sm64-rom $ROOT/baserom.us.z64 \\
  --p2p-bind-ip 0.0.0.0 --p2p-bind-port $P2P_PORT \\
  --rpc-bind-ip 127.0.0.1 --rpc-bind-port 16481 --restricted-rpc \\
  --sm64-solo --no-igd --non-interactive --log-file $DATA/node.log
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
UNIT
sudo systemctl daemon-reload
sudo systemctl enable --now sm64coin-gateway

if command -v ufw >/dev/null; then
  sudo ufw allow OpenSSH >/dev/null; sudo ufw allow "$P2P_PORT/tcp" >/dev/null; sudo ufw --force enable >/dev/null
fi

IP=$(curl -fsS https://api.ipify.org 2>/dev/null || hostname -I | awk '{print $1}')
cat <<MSG

Gateway running (systemd: sm64coin-gateway; log: $DATA/node.log).
  p2p:  $IP:$P2P_PORT  (open this port in your provider's firewall too, if it has one)
  RPC:  127.0.0.1:16481 on this server only
Next, on your own machine:
  GATEWAY=$IP:$P2P_PORT scripts/start-node.sh   # your node talks only to the gateway
and add "$IP:$P2P_PORT" to seeds.txt so every new node finds the network.
Check: nc -vz $IP $P2P_PORT
MSG
