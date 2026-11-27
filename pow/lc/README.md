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
| `proto` | none (wasm ok) | ALPN `sm64coin/light/1`, `Request`/`Response`, `u32 len || postcard` frames, limits, signed `NodeAnnouncement` |
| `nodes` | none (wasm ok) | `NodeList`: scoring, weighted pick, bans, cap 64, JSON persistence, bootstrap parsing |
| `exec`, `allowlist` | none (wasm ok) | `Request` -> node RPC mapping, `Upstream` trait, the RPC allowlist |
| `client` | none / `iroh-client` | `Transport` trait, `UpstreamTransport` (HTTP), `IrohTransport`, `Client` (failover, 3-node submit fan-out) |
| `http` | `http` (`tls`) | ureq `Upstream` |
| `endpoint` | `iroh-client` | endpoint setup: SM64Coin relays first, n0 relays only as a last resort, discovery none/n0/DHT |
| `server` | `server` | `LcService`: iroh `ProtocolHandler`, per-peer + per-IP token buckets, Hello gate, `Busy`, signed `GetNodes`, `Probe` |
| `relay`, `peers` | `relay` | embedded iroh-relay, limits, Hello-gated access, monthly cap, reachability self-test, on/off/auto controller |
| `main.rs` | `bin` | the `sm64coin-lc` binary |

## Status

Done and tested (`cargo test --features relay`, 17 tests): protocol and framing, node list, HTTP and iroh
transports, server against a fake node and against an isolated offline `sm64coind`, relay-only and direct
paths, failover, rate limiting, malformed/oversized frames, relay limits, reachability decision and
controller, signed announcements. Compiles for `wasm32-unknown-unknown` (core, and `iroh-client` with
`CC_wasm32_unknown_unknown` set to a clang and `RUSTFLAGS='--cfg getrandom_backend="wasm_js"'`) and for
`aarch64-linux-android` (client and relay).

Next:
- Integration: launcher depends on `sm64coin-lc` for the allowlist (and starts `serve`), `remote.rs`/`walletproxy.rs` use `Client`, desktop/Android link `iroh-client`, the website uses the wasm wrapper.
- `pow/lc-wasm` (wasm-bindgen wrapper, `fetch` `Upstream`, browser iroh over relay WSS).
- `verify.rs` (checkpoint anchor, header chain, cycle replay, cross-check): interfaces are `Tip`, `Request::GetHeaders/GetBlocks/GetCycle`; `Client::ban_liar` is the hook.
- TLS front on 443 shared with the relay (`RelayService` can be embedded into an HTTP server); ACME is wired (`TlsMode::Acme`, `CertConfig::LetsEncrypt`) but untested against a CA; IP-address certificates depend on iroh-relay's ACME client accepting IP identifiers (unverified).
- Manual TLS certificates are read once (no hot reload); `SubscribeTip`, streamed multi-frame answers and the DHT `get_peers` key (`.../lc`) are not implemented.
