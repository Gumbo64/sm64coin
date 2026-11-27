# Light clients without one public node

Status: implemented in v0.5.5 (v0.4 chain). Everything is plain HTTP(S) to nodes' RPC. An earlier design used iroh (QUIC, relays,
gossip) as the transport; it was built and then removed to keep the code, the dependency tree and the browser download small. It is
archived at the git tag **`archive/iroh`** (the whole branch as it was: transport, service, relays, WASM build, tests).

The website, the Android app and the desktop app (while its own node is down) used to read the chain from one node, `eboy.au`.
They now keep a scored list of nodes, fail over, send submissions to several nodes at once, verify the chain themselves and follow
the tip by push.

## Goals and non-goals

Goals
- No single node anyone depends on: clients find nodes, score them and fail over.
- A random node cannot make a light client believe a false chain, a false balance or a false template without being caught by
  checks the client runs.
- Logic in Rust once: the apps link it, the website runs it as WASM (`pow/lc-wasm`); JS only does I/O and UI.
- The chain tip is pushed (Server-Sent Events), not polled.

Non-goals
- No consensus change. No light-wallet server (the node keeps your view key): wallets keep scanning whole blocks.
- Not anonymity (Tor/I2P are out of scope); privacy is "no worse than Monero's remote-node model".
- No new transport: a node is an HTTP(S) JSON-RPC endpoint, as monerod provides.

## Current state

- `web/public/public-node.json` = `{"url": ..., "nodes": ["https://eboy.au", ...]}` (the old `url` key is still read; entries that are
  not http(s) URLs are skipped). The apps compile the same file in (`desktop/src-tauri/src/remote.rs`); the page cannot widen it.
- `pow/lc` (crate `sm64coin-lc`, its own workspace; the core builds for wasm32): `proto.rs` (requests, answers, limits), `nodes.rs` (the
  scored list), `exec.rs` (request -> the node's JSON-RPC / `.bin` call), `allowlist.rs`, `client.rs` (`Transport`, failover `Client`),
  `http.rs` (ureq `Upstream`, https with the `tls` feature), `block.rs` / `verify.rs` / `checkpoints.rs` / `consensus.rs` / `replay.rs`
  (verification), `tipfeed.rs` + `sse.rs` (the tip service), `main.rs` (`sm64coin-lc`).
- `pow/lc-wasm`: wasm-bindgen wrapper for the page (`fetch` as the `Upstream`, https nodes only).
- Apps: `desktop/src-tauri/src/lcnet.rs` (the `Net`: list, failover, tip hub, discovery), `lcverify.rs`, `walletproxy.rs` (a loopback daemon
  for wallet2 and the Android miner), `remote.rs` (the page's `remote_rpc`).

## Discovery

1. **Bootstrap list**: the release's / site's `public-node.json`. Anyone running a public node can add it by PR.
2. **`/get_public_nodes`** (Apps only) of the nodes already in use: monerod's own list of peers that serve a public RPC (`--public-node`
   puts the RPC port into the peer list; `core_rpc_server.cpp::on_get_public_nodes`). The entries are `ip:rpc_port`, plain http. The
   client keeps only public IP addresses (never loopback, private, link-local or CGNAT ranges: a node cannot point a client at its own
   LAN), at most 8 per answer (`nodes::parse_public_nodes`, `Client::learn`), every ten minutes. A learned node is only a candidate:
   it is chain-checked before use and verified like any other.
3. **By hand**: the node the user confirmed in the Network page, kept above discovered ones.

The browser uses the https entries of the bootstrap list only (an https page cannot fetch plain http: mixed content; a loopback
node is allowed for development). A saved list from an older build keeps its http nodes; records it cannot read (iroh ones) are dropped.

## Scoring and failover

`NodeRecord { addr, source, first_seen, last_ok, ewma_latency_ms, ok_rate (EWMA), height, strikes, banned_until }`. Score = ok_rate x
latency factor x freshness; a node more than 2 blocks behind the median reported height is skipped. Selection is weighted-random among
the top 5, so load spreads. A read that times out (10 s; 60 s for bulk block reads) moves to the next node (up to 4); reads are
idempotent. `submit_block`, proof records and `sendrawtransaction` go to **3** nodes at once. A node caught lying (see Verification)
is banned for a week. The list is capped at 64 records and persisted (`nodes.json` in the app's data dir; localStorage on the site,
which keeps only the bootstrap entries' latency and success history: no bans, heights or extra nodes from storage).

## Website constraints

An https page can only `fetch` https origins, the certificate must be valid for the host and the node must send CORS for the site's
origin (public nodes should allow `*`: restricted RPC holds nothing secret). The CSP `connect-src` allows `https:` and loopback http;
there is no websocket scheme. The tip stream is an `EventSource` (a fetch) to `https://<node>/lc/tip`.

## Verification: what a light client checks

`pow/lc/src/verify.rs` (with `block.rs`, `checkpoints.rs`), the same code native and in WASM. The node is not trusted; only these checks are.
1. **Anchor**: a checkpoint (height, id, cumulative difficulty, weight, last-60 window) compiled into each release; a chain that does not
   contain it is rejected and its node banned.
2. **Header chain**: every block's `prev` link, its id recomputed in Rust, timestamp rules, the recomputed retarget and cumulative
   difficulty, the run length within the time limit, the miner's signature over the committed proof, and the proof weight for the v0.4
   fork choice (longer AND heavier). No emulation.
3. **Cross-check**: up to 5 nodes; the chain each serves is verified from the anchor, equal tips merged, branches ordered by `prefer_alt`;
   a node serving an invalid chain is banned. `quorum` = 3 nodes agree.
4. **Replay** (desktop, `replay` feature): the winning branch's current cycle is replayed with the real perturbation; a branch that fails
   is dropped and its nodes banned. Android and the browser run the header layer.
5. **Templates**: the template's `prev` is the verified tip and its coinbase pays the user's address before any mining starts.
6. **Wallet data**: blocks given to wallet2 must hash to verified headers; tx-pool data stays unverified.

After a check the resume point is kept (`verify::sync_resume`): the next check fetches only newer blocks. When no verified node answers
the apps re-check within 30 s; when every node is banned the best-scored are used anyway and the status says "unverified".

## Privacy

`wallet2` downloads whole blocks and scans locally, so a node does not learn which outputs are yours; it does see your IP, your sync
timing, the `get_outs` request and the transaction you send. The apps and the page publish nothing about themselves and only dial nodes
they know. `sm64_board_join` (sends the view key) is `allowlist::PINNED_ONLY`: only to the node the user picked, never by failover.

## Apps integration

- **Typed and passthrough requests**: the wallet's binary calls, `get_sm64_cycle`, `get_fee_estimate`, `get_block_headers_range`, a plain
  `submit_block` ... are typed `Request`s; every other allowlisted JSON-RPC call is `Request::JsonRpc` and every other wallet path
  `Request::Path`. `exec.rs` maps all of them onto the node's HTTP RPC, so every node is interchangeable. A node with a narrower
  allowlist answers `Unsupported` and the client moves on.
- **Wallet allowlist**, derived from every daemon call wallet2 makes: JSON-RPC `get_version get_info hard_fork_info get_fee_estimate
  get_output_histogram get_output_distribution getblockheadersrange getblockheaderbyheight get_txpool_backlog rpc_access_info`; paths
  `getblocks.bin getblocks_by_height.bin gethashes.bin get_o_indexes.bin get_outs.bin get_output_distribution.bin
  get_transaction_pool_hashes(.bin) get_transaction_pool_stats gettransactions is_key_image_spent send_raw_transaction getheight get_info
  get_public_nodes` (the last for the client's own discovery, not proxied to wallet2). Never the unrestricted ones.
- **Verification in the apps** (`lcverify.rs`): on startup and on each new tip; the Network page and the status-light tooltip show
  `verified (n nodes agree)`, `conflict` or `unverified`; the wallet proxy and the native miner use only the verified branch's nodes.
- **The clock**: a held run is sent and stamped by the NODE's clock (this device's clock corrected by the server time of pushed tips), so
  a phone with a wrong clock does not lose a run to the node's 30 s submit window.

## Tip push (Server-Sent Events)

A miner page used to ask `get_info` every 2 s and a fresh template every 5 s (the time limit grows with the time since the parent).
Now the tip is pushed and the limit is computed on the device.

- **`GET /lc/tip`** (`pow/lc/src/sse.rs`, fed by `tipfeed.rs`): `text/event-stream`; an unnamed event per tip (the JSON record of
  `exec::tip_record`: `node, height, hash, timestamp, cumulative_difficulty, difficulty, server_time`), the current tip first and then each
  change, and `event: ka` every 30 s whose data is the server's clock. `server_time` is stamped when the record is sent.
- **One watcher per server**: `TipFeed` polls the local node (1 s while anybody is subscribed, 5 s otherwise; `get_info`, plus
  `get_last_block_header` only when the tip changed) and fans out through a `watch` channel; a thousand subscribers cost the node what one does.
- **Run it**: `sm64coin-lc serve --sse-bind 127.0.0.1:16490` (systemd unit `packaging/sm64coin-lc.service`), the launcher's `--lc` /
  `--lc-sse-bind`, or the `ssesrv` example. The listener speaks plain HTTP on a local port; the public HTTPS address is the node's reverse
  proxy, which must not buffer: Caddy `reverse_proxy /lc/tip 127.0.0.1:16490 { flush_interval -1 }`; nginx `location = /lc/tip { proxy_pass
  http://127.0.0.1:16490; proxy_buffering off; proxy_read_timeout 1h; }`. The service reads the node's RESTRICTED RPC only (it refuses an
  unrestricted one: no caps there, and Dandelion++ stem transactions in the pool answers).
- **Caps**: 4 subscriptions per client IP (an IPv6 client per /64), 4096 in all; beyond them `429`; open connections 4096 + 256, beyond
  that closed at once. `X-Forwarded-For` (its last entry, the one the proxy adds) is read when the listener is on loopback or with
  `--sse-trust-forwarded`; nginx needs `proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;` (Caddy sets it). A reader that stops
  reading holds only its own task and is dropped when a write stalls 30 s. The browser reconnects after the stream's `retry:` (4-10 s).
- **CORS**: `Access-Control-Allow-Origin: *` (public chain data; an `OPTIONS` preflight is answered `204`).
- **The limit is computed, not fetched**: `rules_max_frames` / `rules_due_elapsed` in `pow/perturb-wasm` compile `pow/sm64pow/src/rules.rs`
  itself, so the page, the apps and the node use one function (`pow/sm64pow/tests/limit_vectors.json` pins the numbers). `limit(now) =
  max_frames(difficulty, now - tip.timestamp)`.
- **Clients choose**: the page (`web/src/lib/tip.js`) tries the app's `tip` event, then SSE at the node in use (a 404 is remembered for the
  page load), then the 30 s poll (plus on focus/visibility and after a submit). A push that ends falls back to the poll after a random 0-5 s
  and is tried again after 60 s doubling to 10 min. In the apps `Net::tip_task` runs `Client::watch_tip` (an HTTP node's
  `Transport::open_tip_stream` is the SSE reader in `http.rs`); `WatchEnd::NoPush` / `NoNodes` end it and the caller polls. The result feeds
  `lcnet::TipHub`: the page's `tip` event, the verifier (one check per new tip) and the Android miner's `TipFeed`.

## Hardening (review)

- **Incremental verification**: nodes claiming a tip inside a verified branch are cut from it (no download); above 64 blocks one claimant
  per distinct tip is verified, the rest spot-checked at 8 random heights. Per-check caps: 500 blocks per node in the browser, 2000 in the apps.
  The checkpoint is regenerated before each release (`scripts/update-checkpoint.sh`, packaging/RELEASING.md).
- **Stored lists are not trusted** (see Scoring). The lag rule compares against the median reported height, not the maximum.
- **Discovery is bounded**: public IPs only, 8 per answer, the list stays at 64 records, bootstrap and manual entries are never pushed out.

## Sources

- Monero public nodes: `monero/src/daemon/main.cpp` (`parse_public_rpc_port`), `monero/src/rpc/core_rpc_server.cpp` (`on_get_public_nodes`)
- The archived iroh design and code: git tag `archive/iroh`
