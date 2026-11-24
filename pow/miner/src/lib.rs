pub mod m64;
pub mod tracker;
#[cfg(not(target_arch = "wasm32"))]
pub mod rpc;
#[cfg(not(target_arch = "wasm32"))]
pub mod beam;
#[cfg(not(target_arch = "wasm32"))]
pub mod routes;
pub mod hook;
pub mod rh;
pub mod rrt;

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
