//! SM64Coin light-client network. See `README.md` and `docs/LIGHT-CLIENTS.md`.
//!
//! Everything runs over plain HTTP(S) to nodes' RPC (the iroh transport is archived at the git tag `archive/iroh`).
//!
//! - [`proto`]: requests, answers and limits (builds for wasm32).
//! - [`nodes`]: scored node list, pure logic.
//! - [`exec`], [`allowlist`]: request -> node RPC mapping and the RPC allowlist.
//! - [`client`]: `Transport` (HTTP(S)), failover `Client`.
//! - [`block`], [`verify`], [`checkpoints`]: light-client verification (anchor, header chain, cross-check; wasm ok);
//!   `consensus` / `replay` features: the rules and the Mario replay from sm64pow.
//! - `tipfeed`, `sse` (`tip-server` feature): the tip service next to a node, Server-Sent Events at `GET /lc/tip`.
pub mod allowlist;
pub mod block;
pub mod client;
pub mod exec;
pub mod nodes;
pub mod proto;

#[cfg(feature = "http")]
pub mod http;

#[cfg(feature = "tip-server")]
pub mod tipfeed;
#[cfg(feature = "tip-server")]
pub mod sse;

pub mod checkpoints;
pub mod verify;
#[cfg(feature = "consensus")]
pub mod consensus;
#[cfg(feature = "replay")]
pub mod replay;
