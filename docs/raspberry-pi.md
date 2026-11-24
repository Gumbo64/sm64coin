# Running a public SM64Coin node on a Raspberry Pi

A public node lets other people join the network: their nodes sync from it
(peer-to-peer, port 16480) and websites / wallets can use its RPC (port 16481).
It verifies every block by replaying its Super Mario 64 run, so it needs the
ROM, and a few minutes of CPU per new cycle when it starts.

## What you need

- Raspberry Pi 4 or 5 with **64-bit** Raspberry Pi OS (Bookworm), 4 GB RAM or more
  (8 GB is comfortable), and ~20 GB free disk (an SSD is better than an SD card).
- The US Super Mario 64 ROM, `baserom.us.z64` (SHA-1 `9bef1128717f958171a4afac3ed78ee2bb4e86ce`).
  It is not distributed with SM64Coin; copy your own onto the Pi.
- Ports 16480 (p2p) and 16481 (RPC) forwarded to the Pi on your router, if it
  should be reachable from the internet.

## 1. Packages and Rust

```sh
sudo apt update
sudo apt install -y git build-essential cmake pkg-config libssl-dev libzmq3-dev \
  libunbound-dev libsodium-dev libunwind8-dev liblzma-dev libreadline-dev libexpat1-dev \
  libpgm-dev libhidapi-dev libusb-1.0-0-dev libprotobuf-dev protobuf-compiler libudev-dev \
  libboost-all-dev python3 ccache
curl https://sh.rustup.rs -sSf | sh -s -- -y && . "$HOME/.cargo/env"
```

A swap file helps the C++ build on a 4 GB Pi:

```sh
sudo dphys-swapfile swapoff && sudo sed -i 's/^CONF_SWAPSIZE=.*/CONF_SWAPSIZE=4096/' /etc/dphys-swapfile \
  && sudo dphys-swapfile setup && sudo dphys-swapfile swapon
```

## 2. Get the code and build

```sh
git clone --recursive https://github.com/Gumbo64/sm64coin.git
cd sm64coin
cp /path/to/baserom.us.z64 .
make build-monero JOBS=2   # the daemon and wallets; ~1-2 h on a Pi 4
make links                 # bin/sm64coind etc.
```

The Rust proof-of-play crate (`pow/sm64pow`) is built by Monero's CMake as part
of the daemon. On ARM the game runs through wasmer's Cranelift compiler (the
precompiled module in sm64-binds is x86-64 only), which is slower but gives the
same results: the daemon checks the game build at startup and refuses any other.

## 3. Run it as a public node

```sh
scripts/public-node.sh          # restricted RPC on 0.0.0.0, browsers allowed (CORS), p2p open
tail -f run/node.log            # wait for "SYNCHRONIZED OK"
curl -s http://127.0.0.1:16481/json_rpc -d '{"jsonrpc":"2.0","id":0,"method":"get_info"}'
```

It finds the network through `seeds.txt` (add `PEERS="host:16480"` to point it
at a node you know). To keep it running across reboots, a systemd unit:

```ini
# /etc/systemd/system/sm64coind.service
[Unit]
Description=SM64Coin node
After=network-online.target

[Service]
User=pi
WorkingDirectory=/home/pi/sm64coin
ExecStart=/home/pi/sm64coin/bin/sm64coind --data-dir /home/pi/sm64coin/data/node --sm64-rom /home/pi/sm64coin/baserom.us.z64 \
  --p2p-bind-port 16480 --rpc-bind-ip 0.0.0.0 --rpc-bind-port 16481 --restricted-rpc --confirm-external-bind \
  --rpc-access-control-origins * --non-interactive
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload && sudo systemctl enable --now sm64coind
```

### Without building: the release bundle

The Linux aarch64 release bundle runs a node without compiling anything: unpack it to `/opt/sm64coin`,
put `baserom.us.z64` there and follow `sm64coin-node.service` (its first lines are the install steps;
`sm64coin-node --help` lists the options, `PUBLIC_RPC=1` serves the restricted RPC on 0.0.0.0:16489).
It runs as user `sm64coin` with data in `/var/lib/sm64coin`.

## 4. Make it the network's seed

Once it's reachable, add its public address to `seeds.txt` (one `host:16480`
per line) and commit, so every new node finds it; the website's Network page
and the GUI wallet can then use `http://<host>:16481` as their node.

## Notes

- The first node of a brand-new network needs `SOLO=1` (it mines without peers);
  a node joining an existing network doesn't.
- Mining on the Pi itself is possible (`make build-pow && scripts/automine.sh`)
  but slow; play in the browser, or run the AutoMiner on a desktop pointed at the Pi
  (`DAEMON_URL=http://<pi>:16481 scripts/automine.sh`).
