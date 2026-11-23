# A gateway node (the network's seed), without publishing your home IP

The gateway is an always-on node on a server with a public IP (a small VPS). It is the address in
`seeds.txt`: every new node finds the network through it. Only its p2p port (16480) is open; its
RPC stays on the server's 127.0.0.1, and it runs no wallet and no miner.

Your own node at home connects **out** to the gateway only (`--add-exclusive-node`), so no other
peer ever sees your home IP and nothing needs to be forwarded on your router. Blocks you mine reach
the network through the gateway, so they appear to come from it.

## 1. A server

Any Linux VPS with a public IPv4 address, 2 cores, 2-4 GB RAM and ~20 GB disk: for example Oracle
Cloud's free ARM instances, or a Hetzner CX22. Ubuntu 22.04/24.04 or Debian 12. Pay and sign up in a
way you're comfortable having linked to the gateway's IP.

## 2. Set it up (on the server)

```sh
git clone --recursive https://github.com/Gumbo64/sm64coin.git && cd sm64coin
scp you@home:baserom.us.z64 .     # your own ROM (every node replays every block to verify it)
scripts/gateway-node.sh           # packages, build (~30-60 min), systemd unit, firewall
```

It installs `sm64coin-gateway.service` (restarts on failure and at boot) and prints the address.
If your provider has its own firewall (Oracle, AWS, ...), open TCP 16480 there too.

Useful commands: `systemctl status sm64coin-gateway`, `tail -f data/gateway/node.log`,
`curl -s 127.0.0.1:16481/json_rpc -d '{"jsonrpc":"2.0","id":0,"method":"get_info"}'` (on the server).

## 3. Point your own node at it (at home)

```sh
GATEWAY=<server-ip>:16480 scripts/start-node.sh
```

Your node then talks only to the gateway (`--add-exclusive-node`, `--hide-my-port`), and the
gateway syncs the chain from it. Check from anywhere: `nc -vz <server-ip> 16480`.

## 4. Publish it

Add `<server-ip>:16480` (or a DNS name pointing at it) to `seeds.txt`, commit and push. New nodes
and the launcher read `seeds.txt` and find the network through the gateway.

## Other ways to hide your IP

- A VPS as a pure relay: a WireGuard (or `ssh -R`) tunnel from home to the VPS, the VPS forwarding
  16480 into the tunnel; the chain stays on your own machine.
- A TCP tunnel service (playit.gg, ngrok TCP): a public host:port for your home node, no VPS.
- Tor (`--proxy 127.0.0.1:9050`) hides your own node's outgoing connections, but it can't be the
  gateway: Monero's onion zone carries transactions, not block sync.
