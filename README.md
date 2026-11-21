# SM64Coin

A Monero fork (v0.18.5.4, git branch `sm64coin`) whose proof of work is **playing Super Mario 64**. A block is sealed by a controller input sequence that, replayed on an emulated N64 from power-on, reaches a star. Every node verifies a block by replaying the proof in an emulator.

Addresses start with `SM64`. Everything else (RingCT, Dandelion++, wallet) is stock Monero.

## Theory

- **The task.** From power-on, play until a star is collected. Success condition and parameters are in `pow/sm64pow/src/rules.rs`.
- **Unforgeable runs.** The controller is randomly perturbed: a xorshift generator seeded from the block, re-mixed with Mario's position and velocity after every frame, injects 5 random bursts of 2 frames per 100-frame window at difficulty 1. Runs cannot be precomputed, recycled between blocks, or cheaply cherry-picked; the miner has to steer around the noise as it goes.
- **The whole block hash is the seed.** xorshift32 holds only 32 bits, which a miner could grind with the nonce to land on a seed they had stockpiled a run for. So the seed is all 256 bits of `cn_fast_hash(hashing blob)`: the RNG is reset to the next 32-bit word of it every 300 frames (`Seed` in `pow/sm64pow/src/lib.rs`). A stockpiled run is worth at most the first 10 seconds.
- **Timestamps increase strictly** (block after parent), so the difficulty decay below measures real time since the parent and can't be gamed by back-dating.
- **Difficulty is hardness.** Each doubling of difficulty cuts the time limit by about 15% (10 min at difficulty 1, floor 1 min) and adds one poke per 100 frames.
- **Retarget.** Monero-style, over 60 blocks, targeting 300 s per block.
- **Liveness.** The effective difficulty halves for every 300 s beyond 600 s since the parent block, so the chain keeps moving if miners disappear.
- **Block id** commits to the *decoded* inputs, independent of codec. The daemon therefore re-encodes submitted proofs to the smallest codec.
- **Genesis coinbase** is burned (paid to the RingCT H point). Coinbase unlock window: 10 blocks.
- The built-in hash miner is disabled.

## Architecture

```
 +-----------------------------+        +------------------------+
 | web/  (Vite + React)        |  /json_rpc          /wallet/json_rpc
 |  Explorer, Block replay,    |------+          +----------------+
 |  Mine (browser), Wallet     |      |          |
 +-----------------------------+      v          v
                              +-------------+  +--------------------+
 | pow/miner  automine  ----->|  sm64coind  |  | sm64coin-wallet-rpc  |
 |  (JSON-RPC: get_block_     |  (monero/,  |  | / sm64coin-wallet-cli|
 |   template, submit_block)  |  branch     |  +--------------------+
 +----------------------------+  sm64coin)  |
                              |      |      |
                              |  src/sm64coin glue (C++)
                              |      | C ABI (ffi.rs)
                              |      v
                              |  pow/sm64pow (Rust): rules, emulator
                              |  replay, proof codecs, retarget
                              +-------------+
```

- `monero/` the fork. Every changed upstream line is marked `SM64COIN` (`grep -rn SM64COIN monero/src`); the glue lives in `monero/src/sm64coin/`. cmake builds `pow/sm64pow` automatically.
- `pow/sm64pow` consensus crate: `rules.rs` (the `Rules` trait: success condition, difficulty to task, retarget, decay; `active()` selects the rules), `proof.rs` (proof codecs), `ffi.rs` (C ABI).
- `pow/miner` the `automine` binary (automated miner) and `trackbench` (offline tracker test).
- `web/` explorer, in-browser replay and manual miner, wallet UI.

Ports: p2p 16480, RPC 16481, ZMQ 16482 (testnet 2648x, stagenet 3648x); wallet RPC 16483; web dev server 5181 (preview 4174).

## Requirements

A US SM64 ROM at `./baserom.us.z64` (sha1 `9bef1128717f958171a4afac3ed78ee2bb4e86ce`, git-ignored), the usual Monero build dependencies, Rust, Node.

## Quick start

```sh
make build                 # monero (cmake + make), rust miner, web
SOLO=1 make node          # first node of a new network (--sm64-solo: counts as synced without peers)
make wallet               # wallet RPC on 127.0.0.1:16483
curl -s http://127.0.0.1:16483/json_rpc -d '{"jsonrpc":"2.0","id":0,"method":"create_wallet","params":{"filename":"miner","password":"","language":"English"}}'
make mine                 # automated mining to the open wallet's address
make mine ADDR=SM64...    # or to an explicit address
make web                  # http://localhost:5181
make stop                 # stops node and wallet RPC (pid files only)
```

Manual mining: open `/mine` in the web UI, upload the ROM once (it stays in the browser), and play. Explore blocks at `/`, replay any block's proof at `/block/:id`, manage funds at `/wallet`.

Joining an existing network: there are no public seed nodes. Run `PEERS="host:16480 host2:16480" make node` (do not set `SOLO`).

For an interactive wallet: `scripts/wallet-cli.sh`.

### Script settings (environment)

`NODE_DATA` (./data/node), `WALLET_DIR` (./data/wallets), `RUN_DIR` (./run: `node.pid`, `node.log`, `wallet-rpc.pid`, ...), `ROM`, `P2P_PORT`, `RPC_PORT`, `ZMQ_PORT`, `WALLET_PORT`, `WEB_PORT`, `DAEMON_URL`, `WALLET_URL`, `SOLO`, `PEERS`, `EXTRA_ARGS` (extra sm64coind flags), `ROUTE` (automine reference route). Extra arguments to `scripts/automine.sh` after the address are passed to `automine` (see `--help`: `--candidates`, `--horizon`, `--commit`, `--blocks`, `--save-dir`).

Other targets: `make test` (`cargo test -p sm64pow`), `make clean-data` (stops and deletes `data/` and this project's pid/log files).

## Changing the rules

Edit `active()` in `pow/sm64pow/src/rules.rs` to return different rules (success condition, difficulty-to-task mapping, retarget, decay). Also update the success-condition mirror in `web/src/lib/rules.js` so the browser miner and replay agree. This is a hard fork: all nodes must run the same rules, and a chain mined under old rules will not validate. Run `make test`, rebuild monero (`make build-monero`) and wipe `data/node` for a fresh network.

## Adding a proof codec

Add an implementation in `pow/sm64pow/src/proof.rs` and register it in `CODECS` (raw and run-length exist today). Because the block id commits to decoded inputs, adding a codec does not change ids; the daemon re-encodes submitted proofs to the smallest registered codec.

## Pulling upstream Monero

```sh
cd monero
git remote add upstream https://github.com/monero-project/monero.git   # once
git fetch upstream --tags
git checkout sm64coin
git rebase v0.18.x.y            # or: git merge v0.18.x.y
```

Conflicts can only occur at lines marked `SM64COIN`; resolve them keeping the marker. Rebuild with `make build-monero`.

## RPC additions

- `get_block_template`: extra `sm64_*` fields describing the task: `sm64_seed_hash` (the 32-byte perturbation seed), `sm64_reseed_frames`, `sm64_seed` (its first word), `sm64_max_frames`, `sm64_random_amount`, `sm64_window_length`, `sm64_burst_length`, `sm64_a/b/z_prob`, `sm64_retarget_difficulty`, `sm64_easier_at` (when a fresh template would be easier), `sm64_rules`, `sm64_goal`. `difficulty` is already the effective (decayed) one. The template blob's last byte is the empty proof (`00`): replace it with `varint(len) + proof`.
- `get_info`: `sm64_*` fields for the task a block found now would face (`sm64_rules`, `sm64_goal`, `sm64_difficulty`, `sm64_reseed_frames`, `sm64_max_frames`, `sm64_window_length`, `sm64_random_amount`, `sm64_burst_length`, `sm64_a_prob`, `sm64_b_prob`, `sm64_z_prob`, `sm64_easier_at`).
- `get_block`: `sm64_pads` (the proof decoded to raw pads, hex, 4 bytes per frame), `sm64_seed_hash`, `sm64_seed`, `sm64_reseed_frames`, `sm64_frames`, `sm64_proof_bytes` and the task the block met (`sm64_max_frames`, `sm64_random_amount`, ...), used by the web replay.
- `submit_block` re-encodes the proof with the smallest codec before storing and relaying it (the block id doesn't change).
- New daemon flags: `--sm64-rom <path>`, `--sm64-solo` (count as synced without peers, to bootstrap a network).
- `submit_block`: accepts a proof in any codec and re-encodes it to the smallest codec before storing.

Check the exact field names with `curl -s http://127.0.0.1:16481/json_rpc -d '{"jsonrpc":"2.0","id":0,"method":"get_info"}'`.

## Caveats

- The ROM is required by the daemon (`--sm64-rom`), the miner and the web UI; it is not distributed.
- The automated miner follows reference routes (`pow/miner/routes/*.m64`, unperturbed runs from objective-rrt) and searches around the noise (`pow/miner/src/tracker.rs`): candidates are simulated through the real perturbation, so whatever it commits verifies. Glitch-heavy routes (hyperspeed, cannon) are hard to follow; `calm-first-star.m64` is the default. It may fail at high difficulty.
- Browser recordings verify natively: a headless-Chromium test recorded 3000-frame runs on 3 seeds with the rendering WASM build and replayed them with the native verifier, matching on every frame (pads forced by the perturbation and Mario's state), including across reseeds. Only that input script was tested.
- `generateblocks` (regtest) can't make valid blocks (no proof); the built-in hash miner is disabled.
- The web miner's success check mirrors `web/src/lib/rules.js`; it must be kept in sync with `rules.rs`.
- Verification replays an emulator run per block, so syncing costs CPU proportional to chain length.
- `--sm64-solo` is only for the first node of a network.

## Leaderboard

An opt-in mining leaderboard (`pow/leaderboard`, binary `sm64coin-leaderboard`, web page at `/leaderboard`). Miners share their address and **private view key**; the service scans coinbase transactions from the daemon and counts the blocks (and rewards) whose coinbase outputs belong to each participant (standard Monero derivation `D = 8aR`, `P_i = Hs(D||i)G + B`, plus view-tag check). It also reports the fewest `sm64_frames` among their blocks.

Privacy: a view key lets the operator see all of your incoming payments (never spend). Anyone can run their own leaderboard; registration verifies `a*G` equals the address's public view key.

```
scripts/leaderboard.sh                 # build + start (pid run/leaderboard.pid, log run/leaderboard.log)
sm64coin-leaderboard --daemon URL --bind 127.0.0.1:16490 --data-dir DIR --poll-secs 10
curl localhost:16490/leaderboard
curl -d '{"name":"me","address":"SM64...","view_key":"<hex>"}' localhost:16490/register
curl -d '{"address":"SM64...","view_key":"<hex>"}' localhost:16490/unregister   # proves ownership
curl localhost:16490/health
```

Registrations and results are stored in `<data-dir>/leaderboard.json`; chain scanning is incremental and rescans from the last common block after a reorg.

## Decentralisation

There is no central server: every node is equal, validates each block by replaying its proof-of-play, and relays valid blocks (full and fluffy) to its peers. There are no hard-coded seed nodes either (Monero's were removed), so a node needs at least one peer address to find the network.

### Seeds (`seeds.txt`)

`seeds.txt` at the project root lists well-known peers, one `host:port` (p2p port, default 16480) per line; `#` comments and blank lines are ignored. `scripts/start-node.sh` passes each entry as `--add-peer` (set `SEEDS_FILE` to use another file, `PEERS="h:p h:p"` to add more from the environment). It is empty now: whoever runs a long-lived node can add it with a pull request. The first node of a brand new network starts with `SOLO=1 scripts/start-node.sh` (`--sm64-solo`, counts as synced without peers); every other node must not.

### Joining

```sh
scripts/join.sh node.example.org:16480 [other:16480 ...]   # starts a node syncing from these peers (+ seeds.txt)
```

Sync verifies every block by replay (about 0.1 to 1 s per block). Notes from testing on one machine: stock Monero accepts only one incoming connection per IP, so several nodes on 127.0.0.1 cannot all connect to the same peer; chain them (node C peers with node B, which peers with node A) or add `--max-connections-per-ip`. Use `NODE_DATA`, `RUN_DIR`, `P2P_PORT`, `RPC_PORT`, `ZMQ_PORT` to run several nodes on one host.

### Public nodes

`scripts/public-node.sh` runs a node others and browsers can use: `--rpc-bind-ip 0.0.0.0 --restricted-rpc --confirm-external-bind --rpc-access-control-origins '*'` (set `ORIGINS=https://my.site` to restrict, comma separated). Restricted RPC serves the public calls (`get_info`, `get_block`, `get_block_template`, `submit_block`, ...) and hides node-admin ones (`stop_daemon`, `pop_blocks`, `get_connections`, ...). Open p2p 16480 and RPC 16481 in the firewall. CORS check: `curl -i -H 'Origin: http://example.com' http://HOST:16481/json_rpc -d '{"jsonrpc":"2.0","id":0,"method":"get_info"}'` must show `Access-Control-Allow-Origin`. A page served over https can only call https nodes: put a TLS reverse proxy (nginx, Caddy) in front of the RPC port.

### Web app on static hosting

The web app needs no server of its own except for the wallet (wallet RPC still goes through the local Vite proxy and is not available on a static site) and the leaderboard. To host the explorer and miner anywhere:

```sh
cd web && npx vite build        # output in web/dist
```

Upload `web/dist` to any static host (GitHub Pages, Netlify, S3, nginx). Serve it from the root of a domain (asset paths are absolute) and make unknown paths fall back to `index.html` (the app uses browser-history routing; on GitHub Pages copy `dist/index.html` to `dist/404.html`). On first visit open `/network`, enter a public node URL (for example `https://node.example.org:16481`), press Test and "Use this node". The choice is stored in the browser (`localStorage` key `sm64coin.node`; empty means the same-origin proxy used in development) and all daemon calls go to that node directly, so the node needs the CORS option above. The page also keeps a saved list of nodes.

### What is still centralised

- No seed nodes exist yet: until someone publishes addresses in `seeds.txt`, you need to be told a peer.
- Public nodes are trusted by the browsers that use them (a lying node can show a false chain); run your own for anything that matters. The wallet RPC trusts its daemon the same way.
- The mining reward and difficulty rules are fixed in the daemon (hard fork to change); there is no on-chain governance.
- The leaderboard service and the project's own web hosting are single servers; they do not affect consensus.
- Blocks are only as unpredictable as the play: a strong player or bot with a fast search can mine most blocks (see Caveats).

## Desktop wallet (GUI)

`gui/` is a fork of the Monero GUI wallet (v0.18.5.4, branch `sm64coin`, edits marked `SM64COIN`) built against
this repo's `monero/` fork (`gui/monero` is a symlink): "SM64Coin Wallet", SM64 ticker and ports, `sm64coin:` payment
URIs, no price fetching or update checks, and a Mining page that points to the web miner and `automine` (there is no
hash miner). Build and run instructions: `gui/SM64COIN.md`. Binary: `bin/sm64coin-wallet-gui`; in the setup
wizard pick a custom node such as `127.0.0.1:16481`.

## Writing your own bot

See `BOTS.md` (and the "Build a bot" page in the web app): the exact mining contract, a Rust SDK (`pow/sdk`,
`MiningEnv` + `Bot` trait + `run_miner`) and a Python SDK (`python/`), offline practice, and how to check a proof
with the real verifier.
