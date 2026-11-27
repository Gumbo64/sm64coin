# sm64coin-lc: the light-client network

Design: `docs/LIGHT-CLIENTS.md`. Everything is plain HTTP(S) to nodes' RPC. (An iroh transport with relays existed; it is archived at
the git tag `archive/iroh`.) This crate is its own Cargo workspace (own `Cargo.lock`, like `pow/launcher`) and needs no consensus
crates unless you ask for the `consensus` feature.

## Run beside a node

```sh
cargo build --release --features bin
# a node with a restricted RPC on 127.0.0.1:16489 is already running (sm64coind --rpc-restricted-bind-port 16489)
./sm64coin-lc serve --node-rpc http://127.0.0.1:16489 --sse-bind 127.0.0.1:16490
```

It serves the chain tip as Server-Sent Events at `GET /lc/tip` on that local port; your HTTPS reverse proxy forwards `/lc/tip` to it
with buffering off (see `src/sse.rs`, `packaging/sm64coin-lc.service`). Flags: `--allow-unrestricted-rpc` (closed test networks only),
`--sse-trust-forwarded`. `sm64coin-lc ping <https://node>` prints a node's tip. Only the allowlisted node RPC methods
(`src/allowlist.rs`) are ever asked.

## Layout and features

| Module | Feature | What |
|---|---|---|
| `proto` | none (wasm ok) | `Request` / `Response` (typed requests plus `JsonRpc` / `Path` passthrough), `Tip`, `TipInfo`, limits |
| `nodes` | none (wasm ok) | `NodeList`: scoring, weighted pick, bans, cap 64, JSON persistence (unreadable records dropped), bootstrap parsing, `/get_public_nodes` parsing |
| `exec`, `allowlist` | none (wasm ok) | `Request` -> node RPC mapping, `Upstream` trait, the RPC allowlist |
| `client` | none (`http` for `watch_tip`) | `Transport` trait, `UpstreamTransport`, `Client` (failover, 3-node submit fan-out, tip watch) |
| `http` | `http` (`tls`) | ureq `Upstream`, the SSE tip reader |
| `tipfeed`, `sse` | `tip-server` | one watcher of the local node, `GET /lc/tip` |
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
`wasm-bindgen 0.2.108` like `pow/webminer`, with sm64-binds' JS backend: `pow/lc-wasm`).

Gaps: coinbase / reward correctness and non-coinbase transactions are not checked (a light client does not validate
money); a node's local "tainted block" equivocation evidence is not part of the weight; the cycle's first block is
replayed from power-on without proving its parent ended the cycle (replay `extra_cycles` or `backfill` to cover it);
restart-delay limits are only enforced inside a replayed span; the replay has not been run in a browser.

## Status

Tested: `cargo test --features tip-server,tls,consensus,replay` (node list, SSE service and client, the request mapping and allowlist
against a fake node and an isolated offline `sm64coind`, the verifier). The apps' failover and verification tests run in
`desktop/src-tauri` (`lcnet_test.rs`).
