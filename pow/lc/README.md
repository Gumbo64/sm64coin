# sm64coin-lc: the light-client network

Design: `docs/LIGHT-CLIENTS.md`. This crate is the core of step 2 (iroh) plus the shared client logic of
step 1. It is its own Cargo workspace (own `Cargo.lock`, like `pow/launcher`), needs no consensus crates,
and pins `iroh` / `iroh-relay` to `=1.5.0`.

## Run beside a node

```sh
cargo build --release --features bin
# a node with its RPC on 127.0.0.1:16481 is already running
./sm64coin-lc serve --node-rpc http://127.0.0.1:16481 --data-dir ~/.sm64coin-lc \
    --relay auto --discovery dht --bootstrap iroh:<id>@https://relay.example
```

It prints its endpoint id. Flags: `--relay on|off|auto`, `--relay-bind` (default `0.0.0.0:443`),
`--relay-rate-kib 256`, `--relay-max-clients 200`, `--relay-monthly-cap-gib 2048` (0 = none),
`--tls self-signed|pem|acme` (`--acme-domain`, `--acme-contact`, `--acme-production`),
`--discovery none|n0|dht`, `--relay-url` (SM64Coin relays, repeatable), `--no-n0-fallback`,
`--public-host`, `--https-rpc`. State in `--data-dir`: `secret.key`, `nodes.json`, `relay-usage.json`, `acme/`.
`sm64coin-lc ping iroh:<id>[@<relay>]` and `cargo run --example client --features relay` are small clients.

Only the allowlisted node RPC methods (`src/allowlist.rs`) are ever forwarded.

## Layout and features

| Module | Feature | What |
|---|---|---|
| `proto` | none (wasm ok) | ALPN `sm64coin/light/1` (minor 1 adds `Request::JsonRpc` / `Request::Path`: any allowlisted node call over any transport; a request variant newer than the build is answered `Unsupported`, never a disconnect), `Request`/`Response`, `u32 len || postcard` frames, limits, signed `NodeAnnouncement` |
| `nodes` | none (wasm ok) | `NodeList`: scoring, weighted pick, bans, cap 64, JSON persistence, bootstrap parsing |
| `exec`, `allowlist` | none (wasm ok) | `Request` -> node RPC mapping, `Upstream` trait, the RPC allowlist |
| `client` | none / `iroh-client` | `Transport` trait, `UpstreamTransport` (HTTP), `IrohTransport`, `Client` (failover, 3-node submit fan-out) |
| `http` | `http` (`tls`) | ureq `Upstream` |
| `endpoint` | `iroh-client` | endpoint setup: SM64Coin relays first, n0 relays only as a last resort, discovery none/n0/DHT |
| `server` | `server` | `LcService`: iroh `ProtocolHandler`, per-peer + per-IP token buckets, Hello gate, `Busy`, signed `GetNodes`, `Probe` |
| `relay`, `peers` | `relay` | embedded iroh-relay, limits, Hello-gated access, monthly cap, reachability self-test, on/off/auto controller |
| `block` | none (wasm ok) | typed block decoding: parse a block blob, recompute its id (Monero hashing + the proof id), `RpcBlock` / `Claims` from `get_block` JSON |
| `verify` | none (wasm ok) | the verifier: `Checkpoint`, `ChainState::extend` (header rules), `verify_branch`, `candidates` / `sync` (cross-check, fork choice, bans), `Judge` hook, `backfill` |
| `checkpoints` | none | the compiled-in anchors (`checkpoints/mainnet.json`, made by `examples/checkpoint.rs`) |
| `consensus` | `consensus` | `Sm64Consensus`: difficulty, run limit, signatures, proof weight, fork choice, all called from sm64pow |
| `replay` | `replay` | `Sm64Replay`: replays Mario runs with sm64pow's verifier; budgeted and cancellable |
| `main.rs` | `bin` | the `sm64coin-lc` binary |

## Verification (what a light client checks)

Full design: `docs/LIGHT-CLIENTS.md`. Nothing a node says is trusted; the layers, cheapest first:

1. **Anchor**: `checkpoints::mainnet_latest()` (height, id, cumulative difficulty, weight, last-60 window).
   A chain that does not contain it is rejected and its node banned.
2. **Header chain** (`verify_branch`, `ChainState::extend`), per block, no emulation: `prev_id` links; the id is recomputed
   from the blob; timestamp after the parent, at or above the launch floor, at least the median of the last 60, not far
   ahead of the clock; the retarget difficulty and cumulative difficulty are recomputed (and must equal the node's
   claims); the run length is within the time limit; the miner's signature over the committed proof verifies; the
   included proofs are counted for the fork-choice weight (`1 + counted` per block). Rules come from sm64pow through
   `Consensus` (`consensus` feature); without it `StructuralOnly` checks linkage, ids and timestamps and reports
   `Level::Structural`.
3. **Cross-check** (`sync` / `candidates`): ask up to 5 nodes, verify the chain each serves from the anchor, merge equal
   tips, order the branches by sm64pow's `prefer_alt` (longer AND heavier). A node serving an invalid chain, a different
   chain than the checkpoint's, or tampered records is banned (`Client::ban_liar`). `Outcome::quorum_met` tells
   whether 3 nodes agree on the winning tip.
4. **Replay** (`Judge`, `replay` feature): header checks cannot tell a real run from a committed hash, so the winning
   branch's current cycle is replayed (`Sm64Replay`, optionally `extra_cycles`, `max_blocks`, `max_duration`, a cancel
   flag, a progress callback). A branch that fails is dropped and its servers banned, then the next is judged.
   `backfill(lo, hi)` fetches authenticated history between two checkpoints for the random-old-cycle spot check
   (`Sm64Replay::replay_history`).

Cost, measured on the live chain (16 blocks, 45 kB): the header layer is about 0.1 ms per block natively in release (1.7 ms for the 16 blocks; 24 ms per block in a debug build)
(signature check dominates), downloads the blocks once per asked node (about 3 kB per block
today); replay of the 16-block cycle (60,929 frames) took 2.4 s natively. Missing: see Gaps below.

```rust
let cp = sm64coin_lc::checkpoints::mainnet_latest();
let rules = sm64coin_lc::consensus::Sm64Consensus;
let replay = sm64coin_lc::replay::Sm64Replay::new(ReplayBudget { cancel: Some(flag), ..Default::default() });
let out = sm64coin_lc::verify::sync(&client, &cp, &rules, Some(&replay), &CrossCheckOptions::default(), &|| flag.load(..)).await?;
// out.best.tip, out.quorum_met, out.banned, out.replay
```

Release routine: `cargo run --example checkpoint --features http,tls,consensus -- <your own node> --depth 36` rewrites
`checkpoints/mainnet.json` (genesis plus one 36 blocks below the tip).

Features: `consensus` and `replay` link sm64pow (and the game crate, sm64-binds). Builds checked: native, `aarch64-linux-android`
(all features), `wasm32-unknown-unknown` (core always; `consensus` and `replay` from a crate whose lockfile pins
`wasm-bindgen 0.2.108` like `pow/webminer`, with sm64-binds' JS backend; in this crate's own lock, which iroh holds at a
newer wasm-bindgen, they do not build for wasm32).

Gaps: coinbase / reward correctness and non-coinbase transactions are not checked (a light client does not validate
money); a node's local "tainted block" equivocation evidence is not part of the weight; the cycle's first block is
replayed from power-on without proving its parent ended the cycle (replay `extra_cycles` or `backfill` to cover it);
restart-delay limits are only enforced inside a replayed span; the replay has not been run in a browser.

## Status

Done and tested (`cargo test --features relay,replay`: 17 + verification tests): protocol and framing, node list, HTTP and iroh
transports, server against a fake node and against an isolated offline `sm64coind`, relay-only and direct
paths, failover, rate limiting, malformed/oversized frames, relay limits, reachability decision and
controller, signed announcements. Compiles for `wasm32-unknown-unknown` (core, and `iroh-client` with
`CC_wasm32_unknown_unknown` set to a clang and `RUSTFLAGS='--cfg getrandom_backend="wasm_js"'`) and for
`aarch64-linux-android` (client and relay).

Next:
- Integration: launcher depends on `sm64coin-lc` for the allowlist (and starts `serve`), `remote.rs`/`walletproxy.rs` use `Client`, desktop/Android link `iroh-client`, the website uses the wasm wrapper.
- `pow/lc-wasm` (wasm-bindgen wrapper, `fetch` `Upstream`, browser iroh over relay WSS).
- Apps: call `verify::sync` after `Client` setup (on a worker thread when a `Judge` is given), show `Outcome` (best tip, `quorum_met`, banned nodes), and keep the ROM available for `Sm64Replay::init`. The verifier fetches whole blocks with `GetBlocks`; `GetHeaders` carries too little to recompute ids.
- TLS front on 443 shared with the relay (`RelayService` can be embedded into an HTTP server); ACME is wired (`TlsMode::Acme`, `CertConfig::LetsEncrypt`) but untested against a CA; IP-address certificates depend on iroh-relay's ACME client accepting IP identifiers (unverified).
- Manual TLS certificates are read once (no hot reload); `SubscribeTip`, streamed multi-frame answers and the DHT `get_peers` key (`.../lc`) are not implemented.
