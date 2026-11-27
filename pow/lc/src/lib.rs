//! SM64Coin light-client network. See `README.md` and `docs/LIGHT-CLIENTS.md`.
//!
//! - [`proto`]: wire messages, framing, limits (no dependencies on iroh: builds for wasm32).
//! - [`nodes`]: scored node list, pure logic.
//! - [`exec`], [`allowlist`]: request -> node RPC mapping and the RPC allowlist.
//! - [`client`]: `Transport` (HTTP, iroh), failover `Client`.
//! - `server`, `relay`, `endpoint` (features): the service next to a node, the embedded relay, endpoint setup.
pub mod allowlist;
pub mod client;
pub mod exec;
pub mod nodes;
pub mod proto;

#[cfg(feature = "http")]
pub mod http;

#[cfg(feature = "iroh-client")]
pub mod endpoint;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "relay")]
pub mod relay;
#[cfg(feature = "relay")]
pub mod peers;
