//! SM64Coin launcher library: everything the `sm64coin` command and the desktop app share.
//! `rom` finds/verifies the ROM, `node` runs sm64coind, `rpc` is the whitelisted JSON-RPC,
//! `seeds`/`dht` find peers, `proc`/`util` are child-process and path helpers.

#[cfg(feature = "desktop")]
pub mod dht;
#[cfg(feature = "desktop")]
pub mod node;
#[cfg(feature = "desktop")]
pub mod proc;
#[cfg(feature = "desktop")]
pub mod rom;
pub mod rpc;
#[cfg(feature = "desktop")]
pub mod seeds;
pub mod util;

pub use util::version;
