# Light clients without one public node

Status: design, nothing implemented. Date 2026-11-27, against v0.5.3 (v0.4 chain).

The website, the Android app and the desktop app (while its own node is down) read the chain from one
node, `eboy.au`: a Raspberry Pi behind a Cloudflare tunnel. The tunnel is to go: the router forwards
443 and 16480 and the Pi serves TLS itself. This document makes the light clients use many nodes:
**step 1** with the RPC nodes already speak, **step 2** with iroh as the light-client transport.

## Goals and non-goals

Goals
- No single node anyone depends on: clients find nodes themselves, score them and fail over.
- A random node cannot make a light client believe a false chain, a false balance or a false template
  without being caught by checks the client runs.
- NAT'd nodes can serve light clients and browsers can reach them (step 2).
- Logic in Rust once (`rust-primary-language`): native apps link it, the website runs it as WASM; JS only
  does I/O and UI.
- iroh group mining (tree-sync records) later reuses the same endpoint.

Non-goals
- No consensus change. Everything here is node policy, client code and a new off-chain protocol.
- No light-wallet server (Monero LWS model, the node keeps your view key): wallets keep scanning whole blocks.
- Not anonymity: Tor/I2P stay out of scope; privacy is "no worse than Monero's remote-node model".
- Not replacing the monerod p2p protocol between full nodes.

## Current state

- `web/public/public-node.json` = `{"url": "https://eboy.au"}`, read at load by `web/src/lib/rpc.js`
  (`resolveNode`): visitors without a chosen node, and Android always, use it. The desktop app compiles the
  same file into `desktop/src-tauri/src/remote.rs` (`public_node()`); `remote_rpc` reaches only that node or
  one https node the user confirmed (`remote-node.txt`), through `pow/launcher/src/rpc.rs::call_url`
  (https only, no redirects, `RPC_ALLOWED` whitelist).
- The wallet (monero-ts `wallet2` in WASM) talks to `daemonUri()` directly from the webview/browser, not
  through Rust: `getblocks.bin`, `get_outs.bin`, `sendrawtransaction` and friends.
- Full-node discovery: `pow/launcher/src/dht.rs` announces the p2p port on mainline DHT under
  SHA-1(`sm64coin-mainnet-v0.3`), plus `seeds.txt` (`p2p.eboy.au:16480`). There is no RPC-node discovery.
- monerod already has Monero's public-node mechanism: `--public-node` (requires restricted RPC,
  `src/daemon/main.cpp::parse_public_rpc_port`) puts the RPC port into the p2p peer-list entry, and the
  unrestricted URI `/get_public_nodes` (`core_rpc_server.cpp::on_get_public_nodes`) returns white/gray peers
  with a nonzero `rpc_port` as `host:port`. Announced addresses are the peer's observed IP, plain http, no
  hostname, no TLS flag.
- The light clients trust the node fully (WEB-SECURITY.md finding 5). The one check is the genesis hash
  (`nodechain.js`). Client-side block replay exists (`web/src/lib/replay/`, perturb/statecodec WASM,
  bit-identical with `get_sm64_cycle`) but only for viewing.

## Step 1: many public nodes, chosen by clients

### Discovery

Sources, merged into one candidate list:
1. **Bootstrap list** shipped with the release and the site: `public-node.json` becomes
   `{"nodes": ["https://eboy.au", ...]}` (old `url` key still read). Anyone running a public node can add it by PR.
2. **`/get_public_nodes`** of every node the client already uses (white list first). Returns `ip:rpc_port`.
3. **Mainline DHT, apps only**: nodes started with `--public-node` also announce their restricted RPC port
   under a second key, SHA-1(`sm64coin-mainnet-v0.3/rpc`) (`dht::NETWORK_RPC`, same `announce_forever`).
   Desktop and Android (Rust, UDP) look it up; browsers cannot speak the DHT.
4. Nodes the user added by hand (Network page), kept above discovered ones.

A candidate is probed before use: `get_info` with the genesis hash and `nettype`, version not below the
client's minimum (from `get_version`), not on another chain (`probeChain` logic, moved to Rust).

### Scoring and failover

`NodeRecord { url, source, first_seen, last_ok, ewma_latency_ms, ok_rate (EWMA), height_lag, strikes,
banned_until, tls: Option<bool> }`. Score = ok_rate x latency factor x freshness, minus height lag (a node
more than 2 blocks behind the best tip seen among candidates is skipped). Selection is weighted-random
among the top 5, so load spreads instead of everyone picking the fastest node. A read that times out
(10 s; 60 s for `getblocks.bin`) moves to the next node; reads are idempotent, so the retry is transparent.
`submit_block`, proof records and `sendrawtransaction` go to **3** nodes at once (censorship by one node
does not matter). A node caught lying (a block whose id does not hash, a template on a parent that is not
the verified tip, a proof that does not replay; see Verification) is banned for a week, not just scored
down. The list is capped at 64 records and persisted (`nodes.json` in the data dir; IndexedDB on the site).

### Website constraints

An https page can only `fetch` https origins (mixed content; loopback excepted), the certificate must be
valid for the host, and the node must send CORS for the site's origin. So the site uses only candidates
that answer `https://`: bootstrap entries, plus discovered `ip:port` that pass an https probe. That last
case is real now that Let's Encrypt issues IP-address certificates (GA 2026-01-15, "shortlived" profile,
160 h lifetime; validated by http-01 or tls-alpn-01, so port 80 or 443 must be reachable). Public nodes
should allow CORS `*`: restricted RPC holds nothing secret, and a fixed origin list breaks whenever the
site moves origin (WEB-SECURITY.md section 8).

### TLS on nodes (and the Pi without the tunnel)

monerod's own `--rpc-ssl` reads cert files but cannot reload them, and 6-day IP certs would mean restarts.
Instead the launcher runs a small **Rust TLS front**: rustls with ACME (domain or IP identifier,
`instant-acme`/`rustls-acme`, hot cert swap), on 443, forwarding the RPC whitelist to the restricted RPC on
127.0.0.1. It is also where per-IP rate limits live: monerod sees every proxied client as 127.0.0.1 and,
since commit `c68fd5c42` in the fork, never bans loopback. The Pi becomes the first user: `sm64coin node
--public --tls-domain eboy.au` (or `--tls-ip`). In step 2 the same port also serves the iroh relay (below).

### What changes where (step 1)

| Piece | Change |
|---|---|
| `pow/lc` (new crate `sm64coin-lc`, no desktop deps, builds for wasm32) | `nodes.rs`: `NodeRecord`, `NodeList` (merge, probe results, score, pick, ban, serialize). `client.rs`: `Transport` trait, `HttpRpc` impl, failover and fan-out. Pure logic; I/O via a trait so the browser plugs in `fetch`. |
| `pow/lc-wasm` (new) | wasm-bindgen wrapper of `NodeList`/selection for `web/`. |
| `pow/launcher` | `dht.rs`: `NETWORK_RPC` key, announce when `--public`. `node.rs`: `--public` adds `--public-node --restricted-rpc` and starts the TLS front (`tlsfront.rs`, feature `tls-front`). `rpc.rs::call_url`: accept `http://` for list members (TLS gives no authenticity for an IP anyway; the checks below do); add `/get_public_nodes` and the wallet's `.bin` paths to the whitelist. |
| `desktop/src-tauri/src/remote.rs` | `Remote` allowlist = the `NodeList` (still never a URL passed by the page). New `walletproxy.rs`: a loopback HTTP proxy (`127.0.0.1:<random>`, desktop and Android) that forwards the wallet's whitelisted calls to the current node with failover, so `wallet2` gets one stable `http://127.0.0.1` daemon (loopback is allowed under mixed content and the CSP already allows `http://127.0.0.1:*`). |
| `web/src/lib/rpc.js` | Asks `lc-wasm` which node to use and reports results back; no scoring logic in JS. Network page lists the candidates and scores. |
| monero fork | Nothing required. Optional: a `tls` bit in the peer-list entry (p2p change) instead of the https probe. |
| `scripts/public-node.sh`, `docs/raspberry-pi.md` | Use `sm64coin node --public`; port-forward 443 + 16480; Cloudflare tunnel removed. |

## Step 2: iroh as the light-client transport

iroh 1.x (1.0 on 2026-06-15, now 1.5.0, 2026-11-19; wire-stable within major 1): an `Endpoint` dialed by
its `EndpointId` (an ed25519 public key), QUIC with hole punching and multipath, relays as the fallback
path, `iroh::protocol` routing by ALPN. Address lookup: `address_lookup::pkarr` (`PkarrPublisher`,
`PkarrResolver`), `address_lookup::dns` (not in browsers), and mainline DHT in the separate crate
`iroh-mainline-address-lookup` (0.6.1). The browser build (`wasm_browser`) is relay-only: no UDP, every
byte goes over a WebSocket to an https relay. `iroh-relay` 1.5.0 has a full server (`server` feature) with
`CertConfig::LetsEncrypt { acme_config, .. }` or `CertConfig::Manual`, QUIC address discovery, per-client
`ClientRateLimit { bytes_per_second, max_burst_bytes }` and an `AccessControl` hook. iroh-gossip is 0.103.0
(not 1.x yet). n0's public relays are rate-limited and supported for 1.x until end of life.

Why iroh and not just more https nodes: a NAT'd or CGNAT node (most home nodes, never phones) can serve
light clients through someone else's relay; every answer is end-to-end authenticated to an `EndpointId`
(a relay or a hostile network cannot alter it, unlike plain http); one protocol for apps, browsers and
later group mining.

### Service protocol (`sm64coin-lc`, `proto.rs`)

ALPN `sm64coin/lc/1`; the major version is in the ALPN (a breaking change is `/2`, served side by side for
a release). One bi-stream per request; frames are `u32 length || postcard(Message)`, max 8 MiB per frame,
large answers streamed as several frames ending with `End`.

| Request | Answer | Notes |
|---|---|---|
| `Hello { minor, features: u64, network: [u8;32] }` | `Hello { minor, features, network, height, tip, relay_url? }` | `network` = genesis hash; mismatch closes the connection. Unknown features ignored. |
| `GetTip` / `SubscribeTip` | `Tip { height, hash, weight }` (subscribe: uni-stream of them) | Replaces `get_info` polling. |
| `GetHeaders { from, count <= 2000 }` | `Headers(Vec<BlockBlob without txs>)` | Header + miner tx + tx hashes + proof tail: enough to recompute the id and replay. |
| `GetBlocks { from, count, max_bytes }` | `Blocks(Vec<{block, txs}>)` | Whole blocks, the `getblocks.bin` equivalent for `wallet2`. |
| `GetCycle { hash }` | `Cycle { blocks, played_pads }` | As `get_sm64_cycle`. |
| `GetTemplate { address, reserve }` | `Template { blob, height, prev, seed_info.. }` | As `get_block_template`. |
| `SubmitBlock { blob }` / `SubmitProof { record }` | `Accepted` / `Rejected { reason }` | |
| `GetOuts { amounts, indices }`, `GetOutDistribution`, `GetFee`, `SendTx { blob }`, `GetPoolHashes` | as RPC | wallet2's needs. |
| `GetNodes { max <= 64 }` | `Nodes(Vec<NodeAnnouncement>)` | Peer exchange of light-client servers. |
| any | `Error { Busy { retry_ms } \| TooLarge \| Unsupported \| NotFound }` | |

`minor` and `features` (bits: `CYCLE`, `PROOF_RECORDS`, `TREE_SYNC`...) evolve without a new ALPN.
The server side is `pow/lc/src/server.rs` (feature `server`): an iroh `ProtocolHandler` that answers from
the local node's RPC on 127.0.0.1. It runs in the launcher process next to `sm64coind`, so monerod needs no
change; a C-ABI path into the daemon is only worth it if the RPC bridge is measurably too slow.

### Discovery records

`NodeAnnouncement { endpoint_id, relay_url?, https_rpc?, height, flags (RELAY, LC, ARCHIVE), version,
expires }`, signed by the endpoint key, so a forwarding node cannot alter it. Published three ways:
pkarr to mainline DHT through `iroh-mainline-address-lookup` (the endpoint's own record, holding
`relay_url` and the flags as `UserData`), which answers "where is endpoint X"; a third `get_peers` key,
SHA-1(`sm64coin-mainnet-v0.3/lc`), under which servers announce their QUIC port, which answers "which
endpoints exist" (the client dials the address, and `Hello` plus the signed announcement give the
endpoint id; BEP-44 items are keyed per public key, so they cannot list many nodes under one key); and
`GetNodes` gossip between clients and servers. The browser cannot do DHT: it starts from the bootstrap
list (which gains endpoint ids and relay URLs), asks `GetNodes`, and resolves via the n0 pkarr HTTP
resolver as a fallback.

### Relays on every reachable node

`pow/lc/src/relay.rs` (feature `relay`) wraps the `iroh-relay` server.
- **Auto-enable**: on start, a node with a public address asks two peers it already talks to (a `Probe`
  request on `sm64coin/lc/1`) to connect back to `http(s)://<observed ip>:443` (and the QAD port). Two
  successes from different /16s enable the relay; a failure keeps it off and retries every 6 hours.
  NAT'd/CGNAT nodes, Android and the desktop app on laptops (unless the user opts in) never relay.
- **TLS for browsers**: `CertConfig::LetsEncrypt` with the node's domain if it has one, else an IP
  certificate (shortlived profile, tls-alpn-01 on 443). The relay shares 443 with the step 1 TLS front: one
  listener, routed by path (`/relay` to the relay, `/json_rpc` and friends to RPC).
- **Limits** (defaults, open question 3): `ClientRateLimit` 256 KiB/s with 1 MiB burst per client, 200
  clients, a monthly byte cap after which the node stops advertising `RELAY`. `AccessControl` admits only
  endpoints that sent a valid `Hello` for our network within the last hour, so the relay is not a free
  relay for unrelated iroh apps.
- **Fallback relays**: the Pi's relay and n0's public relays are in every client's `RelayMap`.
  The endpoint's home relay is its own relay if it runs one, else the lowest-latency one.

### Clients

- **Desktop and Android**: `pow/lc` linked into `src-tauri` with the native iroh endpoint (direct QUIC
  where possible). `remote_rpc` and `walletproxy.rs` move from `HttpRpc` to `IrohLc` behind the same
  `Transport` trait; http nodes stay as a second transport.
- **Browser**: `pow/lc-wasm` with iroh's `wasm_browser` build, in a dedicated worker. Every connection goes
  browser to a relay over WSS, then to the node (on the node's home relay). This frees the site from the
  https-node constraint: any node whose home relay has a browser-valid cert is reachable, NAT'd or not. The
  wallet engine gets its daemon through a small shim in the monero-ts patch (its HTTP client calls the
  worker instead of `fetch`), so `wallet2` works unchanged.

### Verification: what a light client checks

`pow/lc/src/verify.rs` (with `block.rs`, `checkpoints.rs`; implemented, see `pow/lc/README.md`), the same code native and
in WASM. The transport is not trusted; only these checks are.
1. **Anchor**: a checkpoint (height, hash) compiled into each release (`checkpoints.rs`, updated at release
   time from a full node). The client never accepts a chain that does not contain it.
2. **Header chain**: from the anchor, every header's `prev` links, its id is recomputed in Rust (a Rust
   port of the block id hash in `pow/lc/src/block.rs`, checked against the live chain's ids and, with the `replay`
   feature, against `sm64pow::template`), the timestamp and
   frame-limit rules (`rules.rs`: `next_difficulty`, `StarCycle::max_frames`) hold, and fork choice is the
   v0.4 rule (`longer AND heavier`, the weight from the proof records). Cheap: no emulation.
3. **Replay the current cycle in full**: every block since the last power-on, with the real perturbation
   (the existing worker game WASM in the browser; `sm64pow` emulator natively). A cycle is seconds natively,
   a few times that in WASM. A fake tip therefore needs real Mario runs that beat the frame limits.
4. **Spot-check history**: replay one random earlier cycle per session (more on idle desktop), and all
   blocks that hold the user's own outputs. A failure bans the node and tells the user.
5. **Cross-check**: tip agreement with at least 3 nodes from different sources (bootstrap, DHT, gossip);
   on disagreement the client fetches both branches' headers and applies fork choice itself.
6. **Templates**: the template's `prev` is the verified tip and its coinbase pays the user's address
   (parsed in Rust) before any mining starts.
7. **Wallet data**: blocks given to `wallet2` must hash to verified headers, so a node can withhold
   nothing within the verified range; tx-pool data stays unverified (shown as "unconfirmed").

### DoS and abuse

Servers: per-endpoint and per-IP request budgets (token bucket per message type; `GetBlocks` and
`GetCycle` cost most), concurrent streams per connection 8, `Busy { retry_ms }` instead of queueing,
replay-heavy requests (`GetCycle` on an old hash) go through the node's existing replay queue (DOS.md).
`SubmitBlock`/`SubmitProof` get the same admission as RPC. Endpoint ids are free to make, so budgets also
key on the relay-observed or direct IP. Clients: hard frame-size cap, timeouts, at most 64 node records,
signed announcements expire after 24 h, and the whole verification above means flooding junk nodes costs
the client time but never correctness.

### Privacy

The Monero remote-node model is the baseline: `wallet2` downloads whole blocks and scans locally, so the
node does not learn which outputs are yours; it does see your IP, your sync timing, the `get_outs` request
(the real input among its decoys) and the tx you send. Here:
- Keep whole-block sync; never add an output-filtered or view-key API. The chain is small, so the cost is low.
- Ask a **different** node for `get_outs` than the one that syncs blocks, and send each tx to 3 nodes
  other than the `get_outs` node, so no single node sees the whole spend.
- Over iroh a relayed node sees the relay, not your IP; the relay sees your IP but only ciphertext.
- `sm64_board_join` (sends the view key) only to a node the user picked by hand.

## Rollout

| Release | Content |
|---|---|
| v0.5.4 | `pow/lc` (`nodes.rs`, `client.rs` with `HttpRpc`), launcher `--public` (DHT RPC key, `--public-node`), `remote.rs` on the node list, `walletproxy.rs`. Apps fail over between http/https nodes. |
| v0.5.5 | TLS front with ACME (domain + IP). Pi: tunnel removed, 443 + 16480 forwarded. `public-node.json` as a list with 2+ entries; site uses `lc-wasm` selection and https-probed discovered nodes. |
| v0.6.0 | `verify.rs` (anchor, header chain, cycle replay, cross-check) for apps and site, on HTTP first. |
| v0.6.1 | iroh service (`proto.rs`, `server.rs`) in the launcher; apps prefer iroh; relays auto-enable. |
| v0.6.2 | Browser over iroh (`lc-wasm` + relay), wallet shim in the monero-ts patch. |
| v0.7 | iroh group mining (tree-sync records over iroh-gossip) on the same endpoint and relays. |

HTTP RPC is never removed: old apps and third-party wallets keep working.

## Rough effort (agent-days, including tests)

| Piece | Effort |
|---|---|
| `nodes.rs` + `client.rs` + DHT RPC key + launcher `--public` | 2 |
| `remote.rs` change + `walletproxy.rs` (desktop + Android) | 1.5 |
| `lc-wasm` + `rpc.js` integration + Network page | 1.5 |
| TLS front with ACME (domain + IP, reload, rate limits) + Pi migration | 2 |
| `block_id.rs` port with golden vectors | 1 |
| `verify.rs` (header chain, fork choice, checkpoints, replay hooks, cross-check) | 4 |
| iroh `proto.rs` + `server.rs` + client transport | 4 |
| Relay wrapper, reachability test, shared 443, limits, access control | 3 |
| Browser iroh build + wallet shim | 3-5 (wasm build risk) |
| Discovery records (pkarr/DHT, `GetNodes`) | 2 |

## Open questions for the user

1. **Trust anchor**: is a checkpoint compiled into each release acceptable (the client verifies from it
   forward), and how deep should a light client replay: the current cycle only, or the current plus N
   earlier cycles?
2. **Site origin and CORS**: may public nodes default to CORS `*` (restricted RPC only), and does the site
   stay on `gumbo64.github.io` or move to its own origin first (WEB-SECURITY.md finding 1)?
3. **Relay limits**: are 256 KiB/s per client, 200 clients and a monthly byte cap sensible defaults, and
   should relaying be on by default for every reachable node or opt-in?
4. **n0 relays**: keep n0's public relays (rate-limited, run by a company) as a default fallback, or only
   SM64Coin relays plus the Pi?
5. **IP certificates**: is it fine for volunteer nodes to get Let's Encrypt IP certificates by default (it
   needs 443 reachable and publishes the IP in Certificate Transparency logs), or domain-only?
6. **Where the service runs**: a Rust process next to `sm64coind` (this document's choice, no monerod
   change) or inside the daemon through the C ABI?
7. **Android DHT**: may the Android app run mainline DHT lookups (UDP, some battery), or only bootstrap
   list + `GetNodes`?
8. **Tunnel removal order**: remove the Cloudflare tunnel only after the TLS front runs on the Pi and at
   least two other https nodes are in `public-node.json`, or as soon as the Pi serves TLS?

## Sources

- iroh 1.0 announcement (2026-06-15): https://iroh.computer/blog/v1
- iroh 1.5.0 docs (`Endpoint`, `address_lookup`, features): https://docs.rs/iroh/latest/iroh/
- iroh-relay 1.5.0 server (`CertConfig`, `ClientRateLimit`, `AccessControl`): https://docs.rs/iroh-relay/latest/iroh_relay/server/
- iroh-mainline-address-lookup 0.6.1, iroh-gossip 0.103.0: crates.io
- Let's Encrypt, 6-day and IP certificates GA (2026-01-15): https://letsencrypt.org/2026/01/15/6day-and-ip-general-availability
- Monero public nodes: `monero/src/daemon/main.cpp` (`parse_public_rpc_port`), `monero/src/rpc/core_rpc_server.cpp` (`on_get_public_nodes`)


## Apps integration (v0.5.x light-client branch)

- **Passthrough**: protocol minor 1 adds `Request::JsonRpc { method, params }` (answer `Response::JsonRpc`, the node's whole
  JSON-RPC envelope) and `Request::Path { path, body }` (answer `Response::Bin`), both gated by `allowlist.rs`. Apps send every
  allowlisted call that has no typed message this way, so `get_info`, `get_sm64_state`, the miner's template with its `sm64_*`
  extras, `get_block` by hash and the wallet's `/get_transactions`, `/is_key_image_spent`, `/get_o_indexes.bin` fail over across
  HTTP and iroh nodes alike. Older nodes answer `Malformed`/`Unsupported` and the client moves on.
- **Wallet allowlist** is derived from every daemon call wallet2 makes (wallet2.cpp, node_rpc_proxy.cpp): JSON-RPC
  `get_version get_info hard_fork_info get_fee_estimate get_output_histogram get_output_distribution getblockheadersrange
  getblockheaderbyheight get_txpool_backlog rpc_access_info`; paths `getblocks.bin getblocks_by_height.bin gethashes.bin
  get_o_indexes.bin get_outs.bin get_output_distribution.bin get_transaction_pool_hashes(.bin) get_transaction_pool_stats
  gettransactions is_key_image_spent send_raw_transaction getheight get_info`. Never the unrestricted ones.
- **Privacy**: apps use discovery `none` (they publish nothing and resolve nothing; they dial by endpoint id and relay URL).
  Only serving nodes publish. `SM64COIN_LC_DISCOVERY=n0|dht` opts in.
- **Bootstrap**: `web/public/public-node.json` has `url` (old clients) and `nodes`: `https://host` or
  `iroh:<endpoint id>@<relay url>`. The Pi's iroh entry is added at deploy time.
- **Verification in the apps** (`desktop/src-tauri/src/lcverify.rs`): on startup and on each new tip `verify::sync` checks up to
  five nodes from the compiled checkpoint; the Network page and the status-light tooltip show `verified (n nodes agree)`,
  `conflict` or `unverified`; the wallet proxy and the native miner use only the verified branch's nodes; liars are banned.
  Desktop also replays the current cycle in the background (budget 120 s, cancelled by the next tip; a branch that fails bans
  its servers); Android runs the header layer only.

## Hardening for v0.5.5 (review)

- **Incremental verification** (`verify::sync_resume`): a check ends with a resume point `RESUME_MARGIN` (6) blocks below
  the verified tip; the next check fetches only blocks after it (the resume block's id by one header). A node whose chain
  does not contain it re-anchors the whole check at the compiled checkpoint (not a ban). Nodes claiming a tip inside a
  verified branch are cut from it (no download); above `FULL_CHECK_BLOCKS` (64) one claimant per distinct tip is verified,
  the rest spot-checked at 8 random heights. Per-check caps: 500 blocks per node in the browser, 2000 in the apps. The
  browser keeps the resume point in WASM memory only; the apps store it in `lc-verified.json` (desktop with replay starts
  its first check from the checkpoint, the replay needs the cycle's blocks). The checkpoint is regenerated before each
  release (`scripts/update-checkpoint.sh`, packaging/RELEASING.md).
- **Stored lists are not trusted**: the browser keeps from localStorage only the bootstrap entries' latency and success
  history (no bans, heights, sources or extra nodes). The lag rule compares against the median reported height, not the
  maximum, so one node's inflated height cannot make the honest ones look behind.
- **Never stranded**: when no verified node answers, the apps re-check within 30 s; a failed check lifts the restriction;
  when every node is banned the best-scored ones are used anyway and the status says the reads are unverified.
- **Service**: reads only the node's restricted RPC (refuses an unrestricted one: no caps there and Dandelion++ stem
  transactions in the pool answers); 16 connections per IP; relayed requests (no IP) share one budget; answers are charged
  by size; announcements are field-checked, an answer teaches a client at most 8 new nodes, and fresh endpoint ids cannot
  push known servers out of a server's list.
- **Relay self-test**: two peers at known public addresses (the probe connection's own path) in different IPv4 /16 or
  IPv6 /32 networks, observed IP public, not CGNAT, and among the endpoint's own discovered addresses; a self-signed relay
  is never advertised (`--tls acme --acme-domain ... --acme-production`, or `--lc-acme-domain` in the launcher); the
  monthly byte count is written atomically and on shutdown.
- **View key**: `sm64_board_join` is `allowlist::PINNED_ONLY`, never sent by failover.

## Tip push (protocol minor 2)

A miner page used to ask `get_info` every 2 s and a fresh template every 5 s (the time limit grows with the time since the parent):
about 2000 requests an hour per open page. Now the tip is pushed and the limit is computed locally.

- **`Request::SubscribeTip`** (variant 16, feature bit `TIP_PUSH`): the answer stream stays open and carries `Response::TipPush(TipInfo)`
  frames (the current tip first, then one per change: `height`, `hash`, `timestamp`, `cumulative_difficulty`, retarget `difficulty`,
  the server's clock) and a `Response::Keepalive` every 30 s. The client ends it by closing the stream; no frame for 75 s means a dead
  path and a reconnect. An older server answers `Unsupported` (one frame, stream finished), and the client polls instead
  (`Client::watch_tip` returns `WatchEnd::NoPush`). HTTP nodes cannot push: the same.
- **One watcher per server**: `LcService` polls its local node once (1 s while anybody is subscribed, 5 s otherwise; `get_info`, plus
  `get_last_block_header` only when the tip changed) and fans the result out through a `watch` channel, so a thousand subscribers cost the
  node what one does. Limits: 2 subscriptions per endpoint, 4096 in all (`Busy` beyond), a subscription needs the `Hello` first.
  monerod's ZMQ pub feed (`--zmq-pub`, topic `json-minimal-chain_main`) would replace the 1 s local poll; it carries no timestamp or
  difficulty, so a header call per block would stay. Not wired: the poll is local and cheap.
- **The limit is computed, not fetched**: `rules_max_frames` / `rules_due_elapsed` in `pow/perturb-wasm` compile `pow/sm64pow/src/rules.rs`
  itself (`#[path]`), so the page, the apps and the node use one function; `pow/sm64pow/tests/limit_vectors.json` pins the numbers for both
  builds. `limit(now) = max_frames(difficulty, now - tip.timestamp)`; a run's valid-at is `parent timestamp + due_elapsed(difficulty, frames)`.
- **Clients**: the page has one tip store (`web/src/lib/tip.js`: push from lc-wasm `watch_tip` or the app's `tip` event, else `get_info` every
  30 s plus on focus/visibility and after a submit); the desktop/Android Rust side has `lcnet::TipHub` (push or 30 s poll; the app's own node
  is polled locally once a second on desktop) which feeds the page's `tip` event, the verifier (one check per new tip) and the Android
  miner's `TipFeed`. Standalone `automine` against a local node keeps its 2 s local poll.
