# SM64Coin

**[⬇ Download the latest release](https://github.com/Gumbo64/sm64coin/releases/latest)** (node, wallet app and AutoMiner for Linux, Windows and macOS) · [Website](https://gumbo64.github.io/sm64coin/)

A Monero fork (v0.18.5.4, git branch `sm64coin`) whose proof of work is **playing Super Mario 64**. A block is sealed by a controller input sequence that, replayed on an emulated N64 from the previous block's end state, makes the game's next progress point (a star, a Bowser key, the Grand Star). Every node verifies a block by replaying the proof in an emulator.

Addresses start with `SM64`. Everything else (RingCT, Dandelion++, wallet) is stock Monero.

**Reading the code: start at [docs/CORE.md](docs/CORE.md)**: how a block is mined and verified end to end, the core files in reading order, every `SM64COIN` change in the Monero fork, and what is safe to skip.

**Want to help?** [docs/TASKS.md](docs/TASKS.md) lists open tasks that need no decision from the maintainer, each with a goal and a way to check it worked.

## Theory

- **The 70-star cycle.** Each block is the game's next progress point (a new star, a Bowser key, or the Grand Star once there are 70 stars), played on from the **parent block's end state**. The Grand Star ends the cycle; the next block starts from power-on. A power-on start is also allowed after the genesis block, and as a **restart**: the escape for when no miner can do the next star yet. A restart's run is judged against the limit as it stood 3600 s plus 60 s per progress point thrown away (at most 7200 s) earlier, so it is valid only that long after its parent: an upper bound on how long one star can hold the chain up, at every difficulty (`Rules::restart_delay`; set `restart_base_seconds` to `u64::MAX` to turn restarts off once bots can do every star). The chain is thus one long playthrough, one star per block. Rules: `pow/sm64pow/src/rules.rs` (`StarCycle`), chain logic `pow/sm64pow/src/chain.rs`.
- **Proof format.** `varint(frames) || start byte (0 = parent, 1 = power-on) || codec || inputs of the non-poked frames`. Success (`is_success(start_progress, now_progress)`: one more star, key or Grand Star than at the start) must first hold on the last frame. Verification needs the parent's end state: nodes keep recent end states in memory and rebuild missing ones by replaying the cycle (seconds), nothing is stored on disk.
- **Unforgeable runs.** The controller is randomly perturbed: 5 random bursts of 2 frames per 100 game-clock ticks, at every difficulty, each replacing the pad with a random one. Two generators seeded from the whole block hash decide when pokes land and what they press, and a digest of the game state is mixed into each after every simulated frame (`timing_mix`/`pad_mix` in `StarCycle`; per window or never are one constant away). Per frame was chosen by a compute-normalised comparison: under it honest search is already the best use of compute (its own variation rerolls pokes, a dedicated reroller gains nothing), whereas freezing pokes per window let targeted rerolls beat honest search and freezing them per game let nonce screening against a route's harm map erase them. Pokes are drawn only on frames the game simulates, and a poke on a frame it didn't simulate (e.g. the pause menu opening) is refunded and drawn again on the next one; the digest leaves out the frame timer, so pausing neither absorbs nor rerolls pokes. Recorded routes never survived replay under a fresh seed in testing. Design study and evidence: `pow/perturb/DESIGN.md`.
- **The whole block hash is the seed**, all 256 bits of `cn_fast_hash(hashing blob)`; it does not include the timestamp, so a miner stamps the block "now" right before submitting (rewriting the template's timestamp, not fetching a new template).
- **Difficulty is only the time limit.** Each eighth of a doubling cuts it by 2 % (20 min of play at difficulty 1: the longest star of a real 120-star route is 9.8 min and pokes lengthen runs; floor 600 frames); the pokes are fixed.
- **Retarget.** Monero-style, over 60 blocks, targeting 1200 s (20 min) per block.
- **The frame-length auction.** The limit starts at the retarget difficulty's limit the moment the parent is timestamped and grows every second (no grace; one difficulty halving per 60 s), so the shortest run anyone holds becomes valid first, and the first valid block seen wins (`get_block_template` with `sm64_frames: N` returns `sm64_valid_at`). Fork choice counts blocks: every block weighs the same whatever its difficulty or timestamp, the longer chain wins and a tie keeps the chain seen first, so a restamped or re-coded copy of a block only ties it (`Rules::weigh_by_blocks`). Weighing by difficulty would let a private chain stamped as fast as its runs allow pump its weight (a time warp: a run of L frames fits every difficulty whose limit reaches L at the declared gap). The retarget still reads the difficulties. No block but the genesis block may be timestamped before the network's launch (`Rules::launch_timestamp`), so a fork from the genesis block can't declare years of time it never spent. The steep slope keeps block times tight and rewards the shortest run near the block time; a shallow one would act like a fixed threshold (first run in wins). The chain keeps moving if miners disappear. Modelled in `pow/tools/auction` (see its README).
- **Hold policy (nodes, not consensus).** A block extending the tip whose run is valid at its own timestamp but doesn't fit the limit yet by the node's own clock is held, not rejected (its peer isn't punished), and processed and relayed once it fits (`Blockchain::sm64_hold`). Stamping a block ahead of the clock therefore gains nothing; miners stamp "now". Historical sync is unaffected (by then every run fits).
- **Block id** commits to the *decoded* proof (frame count and start included), independent of codec, for the stateless codecs (0 raw, 1 run-length, 2 xz). The daemon therefore re-encodes submitted proofs to the smallest of them. **State-aware codecs** (3 `statectx1`, 4/5 `statemix`, `pow/sm64pow/src/statecodec.rs`, `statemix.rs`; on since the state-codec hard fork, cargo feature `state-codec`) code each input given the game state it is played in, so they are decoded by replaying the run (`get_block` with `sm64_played` fills `sm64_pads`; `decode_raw` cannot). Their encoding is canonical (the verifier re-encodes and compares), the block id commits to the encoded bytes, and the daemon never re-encodes them. Miners seal with `seal_proof_at`, which picks the smallest of all codecs (about 44% smaller than xz on real chain proofs). Browser and Python miners submit raw proofs (the daemon turns them into xz); only the Rust miners and SDK write and read state-coded ones.
- **Genesis coinbase** is burned (paid to the RingCT H point). Coinbase unlock window: 72 blocks (24 h) up to the v0.4.2 activation height, 6 blocks (about 2 h at 20-minute blocks) from it (`sm64coin_mined_money_unlock_window`, `cryptonote_config.h`).
- The built-in hash miner is disabled.

## Architecture

```
 +-----------------------------+   +-----------------------------+
 | web/  (Vite + React)        |   | gui/  SM64Coin wallet (Qt)  |
 |  Explorer, Block replay,    |   |  funds, Mining, Leaderboard |
 |  Mine (browser), Info page  |   |  (opt-in), Play to mine ->  |
 |  no keys, no wallet         |   |  web /mine?address=...      |
 +-------------+---------------+   +--------------+--------------+
               | /json_rpc                        | daemon RPC
               v                                  v
 +----------------------------+  +-------------------------------+
 | pow/miner  automine  ----->|  sm64coind  (monero/, branch     |
 |  (JSON-RPC: get_block_     |  sm64coin)                       |
 |   template, submit_block)  |      |                           |
 +----------------------------+  src/sm64coin glue (C++)         |
   sm64coin-wallet-rpc / -cli    |      | C ABI (ffi.rs)           |
   (scripts, bots) -------------->      v                          |
                                 |  pow/sm64pow (Rust): rules,     |
                                 |  emulator replay, perturbation  |
                                 |  (pow/perturb), proof codecs,   |
                                 |  retarget                       |
                                 +---------------------------------+
```

- `monero/` the fork. Every changed upstream line is marked `SM64COIN` (`grep -rn SM64COIN monero/src`); the glue lives in `monero/src/sm64coin/`. cmake builds `pow/sm64pow` automatically.
- `pow/sm64pow` consensus crate: `rules.rs` (the `Rules` trait: success condition, difficulty to task, retarget, decay; `active()` selects the rules), `proof.rs` (proof codecs), `ffi.rs` (C ABI).
- `pow/miner` the `automine` binary (automated miner: per block it rebuilds the parent's end state from `get_sm64_cycle` and plays on with a rolling-horizon GA over a route library, `src/rh.rs`), `rhbench` (offline benchmark, `--chain N` for further blocks) and `trackbench` (older beam controller).
- `web/` explorer, in-browser replay, manual miner and auto-miner, lite wallet, leaderboard, info page. It holds no keys and needs no wallet: the wallet is the desktop app (`gui/`).
- **Client-side replay.** A visitor who stored the ROM replays blocks in the browser instead of asking a node: `web/src/lib/replay/` fetches each block once with plain `get_block` (the proof is the tail of its blob), and a Web Worker replays it in the headless game build (the one nodes run) with the nodes' own pokes and proof codecs compiled to WASM (`pow/perturb-wasm`, `pow/statecodec`), the same answer as `get_sm64_cycle`, checked bit-identical against a node on the live chain. Results are kept per block hash in IndexedDB. Without a ROM the site asks the node (`get_sm64_cycle`, cached per block on the node).

Ports: p2p 16480, RPC 16481, ZMQ 16482 (testnet 2648x, stagenet 3648x); wallet RPC 16483; web dev server 5181 (preview 4174).

## Download and run (no build)

Releases (https://github.com/Gumbo64/sm64coin/releases) hold one archive per platform (Linux x86_64, Linux aarch64/Raspberry Pi). Unpack it, copy your US ROM next to it as `baserom.us.z64` (or into `~/.sm64coin/`), open the wallet app (`sm64coin-wallet-gui`; `SM64Coin-Wallet.app` on macOS, `sm64coin-wallet-gui.exe` on Windows). It creates or restores a wallet and starts its own node, which syncs from the seed nodes; the Mining page has Start (automated miner) and Play to mine (website). Servers without the wallet app run `sm64coin-node` (a script around `sm64coind`: finds the ROM, resolves `seeds.txt`, `--help` lists the options) and can use `sm64coin-node.service` (systemd); open TCP 16480. The old `sm64coin` launcher is no longer shipped (source kept in `pow/launcher`). Release process and gaps: `packaging/RELEASING.md`.

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

Manual mining: open `/mine` in the web UI, upload the ROM once (it stays in the browser), and play. Explore blocks at `/`, replay any block's proof at `/block/:id`. Funds, mining controls and joining the leaderboard are in the wallet app (`bin/sm64coin-wallet-gui`); its Mining → Play to mine opens `/mine` with your address.

Joining an existing network: nodes find each other through BitTorrent's Mainline DHT (the old launcher, `pow/launcher`, which is no longer shipped, looked up the network key `sm64coin-mainnet-1`; the release now relies on `seeds.txt`), plus `seeds.txt`. To point at a node you know: `PEERS="host:16480 host2:16480" make node` (do not set `SOLO`). 

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
- `get_block_template`, cycle fields: `sm64_start` (`parent`, `power-on` or `either`), `sm64_power_on_at` (when a restart becomes allowed), `sm64_restart_delay` (a restart's limit is the limit this many seconds earlier: its `sm64_valid_at` is the parent-start one plus this), `sm64_parent_timestamp`, `sm64_parent_stars`, `sm64_parent_keys`, `sm64_cycle_start_height`, `sm64_retarget_difficulty`; with the request parameter `sm64_frames: N` also `sm64_valid_at` (unix time from which a run of N frames is valid).
- `get_sm64_cycle {hash?}` (JSON-RPC; default the tip): `{blocks: [{height, hash, frames, power_on}], played_pads, stars, keys, grand_star, ends_cycle}`. `played_pads` is hex, 4 bytes per frame (u16 LE buttons, i8 x, i8 y), every frame's played pad from the cycle's first block to that tip; replaying it unperturbed from power-on reaches the tip's end state, the next block's start state.
- `get_info`: `sm64_*` fields for the task a block found now would face (`sm64_rules`, `sm64_goal`, `sm64_difficulty`, `sm64_reseed_frames`, `sm64_max_frames`, `sm64_window_length`, `sm64_random_amount`, `sm64_burst_length`, `sm64_a_prob`, `sm64_b_prob`, `sm64_z_prob`, `sm64_easier_at`).
- `get_block`: `sm64_pads` (the proof decoded to raw pads, hex, 4 bytes per frame), `sm64_seed_hash`, `sm64_seed`, `sm64_reseed_frames`, `sm64_frames`, `sm64_proof_bytes` and the task the block met (`sm64_max_frames`, `sm64_random_amount`, ...), used by the web replay.
- `submit_block` re-encodes the proof with the smallest codec before storing and relaying it (the block id doesn't change).
- New daemon flags: `--sm64-rom <path>`, `--sm64-solo` (count as synced without peers, to bootstrap a network).
- `submit_block`: accepts a proof in any codec and re-encodes it to the smallest codec before storing.

Check the exact field names with `curl -s http://127.0.0.1:16481/json_rpc -d '{"jsonrpc":"2.0","id":0,"method":"get_info"}'`.

## Caveats

- The ROM is required by the daemon (`--sm64-rom`), the miner and the web UI; it is not distributed.
- The automated miner (`pow/miner`) rebuilds the parent's end state (replaying `get_sm64_cycle`), then plays on with the rolling-horizon GA (`src/rh.rs`) along a small route library keyed by the stars already collected. Today it can make Bob-omb Battlefield's "Behind Chain Chomp's Gate" star (from power-on) and the "Big Bob-omb on the Summit" star (from the castle after the first); for any other state it has no route and waits (a new tip, or the restart delay after which the power-on start is allowed). A winning run over the current limit waits for `sm64_valid_at`. Candidates are simulated through the real perturbation, so whatever it commits verifies.
- Browser recordings verify natively: a headless-Chromium test recorded 3000-frame runs on 3 seeds with the rendering WASM build and replayed them with the native verifier, matching on every frame (pads forced by the perturbation and Mario's state), including across reseeds. Only that input script was tested.
- `generateblocks` (regtest) can't make valid blocks (no proof); the built-in hash miner is disabled.
- The web miner's success check mirrors `web/src/lib/rules.js`; it must be kept in sync with `rules.rs`.
- Verification replays an emulator run per block, so syncing costs CPU proportional to chain length.
- `--sm64-solo` is only for the first node of a network.

## Leaderboard

An opt-in mining leaderboard (`pow/leaderboard`, binary `sm64coin-leaderboard`, web page at `/leaderboard`). Miners share their address and **private view key**; the service scans coinbase transactions from the daemon and counts the blocks (and rewards) whose coinbase outputs belong to each participant (standard Monero derivation `D = 8aR`, `P_i = Hs(D||i)G + B`, plus view-tag check). It also reports the fewest `sm64_frames` among their blocks.

Privacy: a view key lets whoever holds it see all of your incoming payments (never spend). Registration verifies `a*G` equals the address's public view key. Use a dedicated mining address.

**Sharing (off-chain, no consensus involvement).** Leaderboard servers share their top 50 with each other. `GET /share` returns `{"version":1,"id":...,"entries":[{name,address,view_key,blocks,reward_atomic,best_frames}]}` for the top 50 verified entries (the view key is included, so every server in the network can verify; `blocks`/`reward_atomic`/`best_frames` are display only). Each server pulls `/share` from its peers every `--share-secs` (default 300, jittered, so it works behind NAT), and for each entry checks the address and that the view key matches it (`a*G` = public view key), then counts the blocks itself with its own chain scan: stats from peers are never trusted. Entries that fail are ignored; an address already known keeps its local entry/name; at most 100 entries are taken per pull (256 KiB max response), at most 1000 entries and 200 peers are tracked (lowest-scoring peer-learned entries are evicted; local registrations never). `/leaderboard` is the top 50 by blocks, then reward (each row has `source`: `local` or `peer`, and `verified`). `/unregister` also stops that address being re-learned by this server; servers that already learned it keep it. `GET /peers` lists peers; `/health` has `peers` (responding) and `known_peers`.

Peers come from: `--peer-leaderboard URL` (repeatable) and `--peers-file FILE` (`leaderboard-peers.txt`); the node's connected peers (RPC `get_connections`, unrestricted RPC only; tries `http://<ip>:16490`, `--peer-port`, `--no-discover` disables); and servers that pull from us with an `X-Leaderboard-Url` header (`--announce-url`), accepted only if the announced host is the caller's own IP. Unresponsive peers are retried with backoff and dropped. `/share` is rate limited per IP.

Run the server yourself (`sm64coin-leaderboard`, port 16490; `scripts/leaderboard.sh`); the wallet app and the shipped bundle no longer start one for you.

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

`seeds.txt` at the project root lists the seed nodes (today `p2p.eboy.au:16480`), one `host:port` (p2p port, default 16480) per line; `#` comments and blank lines are ignored. `scripts/start-node.sh` and `sm64coin-node` pass each entry as `--add-peer` (set `SEEDS_FILE` to use another file, `PEERS="h:p h:p"` to add more from the environment). Whoever runs a long-lived node that accepts connections can add it with a pull request. The first node of a brand new network starts with `SOLO=1 scripts/start-node.sh` (`--sm64-solo`, counts as synced without peers); every other node must not.

### Joining

```sh
scripts/join.sh node.example.org:16480 [other:16480 ...]   # starts a node syncing from these peers (+ seeds.txt)
```

Sync verifies every block by replay (about 0.1 to 1 s per block). Notes from testing on one machine: stock Monero accepts only one incoming connection per IP, so several nodes on 127.0.0.1 cannot all connect to the same peer; chain them (node C peers with node B, which peers with node A) or add `--max-connections-per-ip`. Use `NODE_DATA`, `RUN_DIR`, `P2P_PORT`, `RPC_PORT`, `ZMQ_PORT` to run several nodes on one host.

### Public nodes

`scripts/public-node.sh` runs a node others and browsers can use: `--rpc-bind-ip 0.0.0.0 --restricted-rpc --confirm-external-bind --rpc-access-control-origins '*'` (set `ORIGINS=https://my.site` to restrict, comma separated). Restricted RPC serves the public calls (`get_info`, `get_block`, `get_block_template`, `submit_block`, ...) and hides node-admin ones (`stop_daemon`, `pop_blocks`, `get_connections`, ...). Open p2p 16480 and RPC 16481 in the firewall. CORS check: `curl -i -H 'Origin: http://example.com' http://HOST:16481/json_rpc -d '{"jsonrpc":"2.0","id":0,"method":"get_info"}'` must show `Access-Control-Allow-Origin`. A page served over https can only call https nodes: put a TLS reverse proxy (nginx, Caddy) in front of the RPC port.

### Website on GitHub Pages

`.github/workflows/pages.yml` builds `web/` on every push to `main` that touches it and publishes it at
`https://gumbo64.github.io/sm64coin/` (one-time setup: Settings -> Pages -> Source: GitHub Actions). The pages are
static and call a node's RPC from the visitor's browser, by default the visitor's own node at
`http://127.0.0.1:16481`, so nothing goes through GitHub and the RPC stays bound to localhost. Nodes started by the
`sm64coin-node` or `scripts/start-node.sh` allow that origin (`--rpc-access-control-origins https://gumbo64.github.io,https://sm64coin.pages.dev`, a comma separated list;
`SITE_ORIGIN` overrides it) and answer Chrome's private-network preflight. Chrome and Firefox let an https page call
`http://127.0.0.1`; newer Chrome asks first ("access devices on your local network": allow). Safari may refuse.
Another node: the site's Network page.

**Public node fallback.** Visitors without a node of their own can use a public one: put its URL in
`web/public/public-node.json` (`{"url": "https://..."}`; empty = none) and push. The site reads it on each load,
tries the visitor's own node first and uses the public one for that visit if it doesn't answer (a node chosen on
the Network page always wins). The public node should serve the **restricted** RPC (`--rpc-restricted-bind-port`,
e.g. 16489) with the site's origin allowed, behind https, e.g. a Cloudflare tunnel:
`cloudflared tunnel --url http://127.0.0.1:16489`. A quick tunnel's `*.trycloudflare.com` URL changes whenever
`cloudflared` restarts (edit the file again); a named tunnel on your own domain keeps a fixed hostname.

### Web app on static hosting

The web app needs no server of its own except a daemon's RPC and (optionally) a leaderboard server. To host the explorer and miner anywhere:

```sh
cd web && npx vite build        # output in web/dist
```

Upload `web/dist` to any static host (GitHub Pages, Netlify, S3, nginx). Serve it from the root of a domain (asset paths are absolute) and make unknown paths fall back to `index.html` (the app uses browser-history routing; on GitHub Pages copy `dist/index.html` to `dist/404.html`). On first visit open `/network`, enter a public node URL (for example `https://node.example.org:16481`), press Test and "Use this node". The choice is stored in the browser (`localStorage` key `sm64coin.node`; empty means the same-origin proxy used in development) and all daemon calls go to that node directly, so the node needs the CORS option above. The page also keeps a saved list of nodes.

### What is still centralised

- One seed node (`p2p.eboy.au`) so far: if it is down, a new node needs to be told a peer (`--peer HOST:PORT`).
- Public nodes are trusted by the browsers that use them (a lying node can show a false chain); run your own for anything that matters. The wallet RPC trusts its daemon the same way.
- The mining reward and difficulty rules are fixed in the daemon (hard fork to change); there is no on-chain governance.
- The leaderboard service and the project's own web hosting are single servers; they do not affect consensus.
- Blocks are only as unpredictable as the play: a strong player or bot with a fast search can mine most blocks (see Caveats).

## Running a public node (Raspberry Pi)

See [docs/raspberry-pi.md](docs/raspberry-pi.md): packages, `git clone --recursive`, `make build-monero JOBS=2`, `scripts/public-node.sh`, a systemd unit, and adding the node to `seeds.txt`.

## Desktop wallet (GUI)

`gui/` is a fork of the Monero GUI wallet (v0.18.5.4, branch `sm64coin`, edits marked `SM64COIN`) built against
this repo's `monero/` fork (`gui/monero` is a symlink): "SM64Coin Wallet", SM64 ticker and ports, `sm64coin:` payment
URIs, no price fetching, a check for new releases on GitHub (can be turned off), and a Mining page that points to the web miner and `automine` (there is no
hash miner). Build and run instructions: `gui/SM64COIN.md`. Binary: `bin/sm64coin-wallet-gui`; in the setup
wizard pick a custom node such as `127.0.0.1:16481`.

## Writing your own bot

See `BOTS.md` for the exact mining contract, a Rust SDK (`pow/sdk`,
`MiningEnv` + `Bot` trait + `run_miner`) and a Python SDK (`python/`), offline practice, and how to check a proof
with the real verifier.

A bot plays the next progress point from the parent block's end state while the controller is randomly poked; the
SDKs handle the pokes, templates, timing and submission, and you write only the play. Shorter runs win (the time
limit grows from the parent's timestamp); try futures on copies (`env.fork()`, `save()`/`restore()`), since a
recorded route won't survive the pokes; check `stop()`, because a new block makes your run worthless.

```rust
use sm64coin_sdk::*;
struct MyBot;
impl Bot for MyBot {
    fn play(&mut self, env: &mut MiningEnv, stop: &dyn Fn() -> bool) -> anyhow::Result<()> {
        while !env.is_over() && !stop() {
            let _played = env.step(GamePad::new(0, 0, 0))?; // a poke may replace the wanted pad
            let _mario = env.state()?;
        }
        Ok(())
    }
}
fn main() -> anyhow::Result<()> {
    run_miner(&mut MyBot, "http://127.0.0.1:16481", "SM64...", "baserom.us.z64")
}
```

```python
from sm64coin import GamePad, practice, run_miner
def bot(env, stop):
    while not env.is_over() and not stop():
        env.step(GamePad(0, 0, 0))
practice(bot, seed=1, difficulty=1)                  # offline
run_miner(bot, "http://127.0.0.1:16481", "SM64...")  # against a node
```

Examples: `cargo run --release -p sm64coin-sdk --example random_bot` (in `pow/`); Python: `pip install -e python`.
