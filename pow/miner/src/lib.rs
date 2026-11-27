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
