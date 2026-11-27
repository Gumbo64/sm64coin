//! SM64Coin light-client network in the browser (`docs/LIGHT-CLIENTS.md`): the website's node access.
//!
//! JS does the I/O it must (`fetch`), Rust does the rest: the scored node list, failover, fan-out of submissions,
//! the iroh transport (browser build: relay-only, WebSocket to the node's home relay), header verification from the
//! compiled-in checkpoint (`sm64coin_lc::verify`; with the `consensus` feature the real rules from sm64pow).
//! `web/src/lib/lightnet.js` is the JS side.
mod core;
#[cfg(target_arch = "wasm32")]
mod fetch;
#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::*;
