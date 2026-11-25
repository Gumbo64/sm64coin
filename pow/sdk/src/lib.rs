//! SDK for SM64Coin mining bots.
//!
//! You write a [`Bot`]: something that, given a [`MiningEnv`], plays Super
//! Mario 64, from where the block starts (power-on, or the parent block's end
//! state) until the game's next progress point (a star, a Bowser key, or the
//! Grand Star). The SDK handles the rest:
//! the controller perturbation (generated outside the game, the consensus contract, taken
//! from `sm64pow` so it cannot drift), block templates from the daemon,
//! interruption when a new block arrives, local verification, signing (v0.3: every
//! block is signed by a per-template miner key) and submission.
//!
//! ```no_run
//! use sm64coin_sdk::*;
//!
//! struct Idle;
//! impl Bot for Idle {
//!     fn play(&mut self, env: &mut MiningEnv, stop: &dyn Fn() -> bool) -> anyhow::Result<()> {
//!         while !env.is_success() && !env.is_over() && !stop() {
//!             env.step(GamePad::default())?; // returns the pad that was really played
//!         }
//!         Ok(())
//!     }
//! }
//!
//! fn main() -> anyhow::Result<()> {
//!     run_miner(&mut Idle, "http://127.0.0.1:16481", "SM64...", "baserom.us.z64")
//! }
//! ```
//!
//! See `BOTS.md` for the contract in full.

mod daemon;
mod env;
mod miner;

pub use daemon::{Cycle, CycleBlock, Daemon, Template, cycle_end_state, get_timestamp, seal, set_timestamp, sign_proof};
pub use env::{MiningEnv, Snapshot, Task};
pub use miner::{Bot, MinerConfig, SignedStore, PracticeReport, practice, run_miner, run_miner_keyed, run_miner_with};
pub use sm64pow::sign::Keypair;
pub use sm64_binds::{GamePad, GameState, SM64Game, buttons};
pub use sm64pow::game::Progress;
pub use sm64pow::{Params, Seed, Verdict, proof};

/// Loads a bot's controller input from a Mupen64 movie (`.m64`, 0x400 byte
/// header) or a raw file: 4 bytes a frame, u16 big-endian buttons, i8 stick x,
/// i8 stick y.
pub fn load_m64(path: &str) -> anyhow::Result<Vec<GamePad>> {
    let bytes = std::fs::read(path)?;
    let body = if bytes.len() >= 0x400 && &bytes[..4] == b"M64\x1a" { &bytes[0x400..] } else { &bytes[..] };
    Ok(body.chunks_exact(4).map(|c| GamePad::new(u16::from_be_bytes([c[0], c[1]]), c[2] as i8, c[3] as i8)).collect())
}

/// Loads the ROM once per process (idempotent). `rom` is a path to the US
/// `.z64`; an empty string falls back to `$SM64COIN_ROM` / `./baserom.us.z64`.
pub fn init_rom(rom: &str) -> anyhow::Result<()> {
    if rom.is_empty() {
        sm64pow::init_default()?;
    } else {
        sm64pow::init(std::fs::read(rom).map_err(|e| anyhow::anyhow!("reading ROM {rom}: {e}"))?)?;
    }
    Ok(())
}
