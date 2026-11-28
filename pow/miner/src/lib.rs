pub mod m64;
#[cfg(not(target_arch = "wasm32"))]
pub mod payout;
#[cfg(not(target_arch = "wasm32"))]
pub mod rpc;
#[cfg(not(target_arch = "wasm32"))]
pub mod update;
pub mod hook;
pub mod rh;
pub mod macros;
pub mod reset;
pub mod signed;
pub mod rrt;
#[cfg(not(target_arch = "wasm32"))]
pub mod campaign;

/// The environment, files and clock the campaign uses: std's natively; in the browser build
/// (the web miner) obrrt's (`obrrt_core::sys`: a per-instance environment, the worker's shared
/// files, web-time), so a step's settings reach the search it runs.
#[cfg(target_arch = "wasm32")]
pub use obrrt_core::sys;
#[cfg(not(target_arch = "wasm32"))]
pub mod sys {
    pub use std::time::Instant;
    pub mod env {
        pub use std::env::*;
    }
    pub mod fs {
        pub use std::fs::*;
    }
}

/// Seconds since the Unix epoch. Use this (and `sys::Instant`) instead of `std::time`: on
/// wasm32-unknown-unknown (the web miner) `std::time::SystemTime::now()` and `Instant::now()` panic
/// ("time not implemented on this platform"), which killed every web worker whose search met its goal
/// (v0.5.0–v0.5.6). `tests/no_std_time.rs` keeps std's clock out of code the browser runs.
pub fn unix_secs() -> u64 {
    #[cfg(target_arch = "wasm32")]
    let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH);
    #[cfg(not(target_arch = "wasm32"))]
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    now.map_or(0, |d| d.as_secs())
}
