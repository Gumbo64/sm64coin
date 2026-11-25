//! SM64Coin proof-of-play.
//!
//! A block is sealed by playing Super Mario 64 from power-on until the success
//! condition holds (by default: Mario has a star). While playing, the
//! controller is randomly perturbed ("poking the robot"), outside the game
//! ([`perturb`]): a 256-bit generator seeded from the whole block hash, mixed
//! with a digest of the game state ([`game`]) after every frame the game
//! simulates, and scheduled on the game's own clock (so pausing neither spends
//! nor rerolls pokes). The game build is stock; [`SM64MiningGame`] bundles it with
//! its perturbation so save states carry both.
//!
//! This crate is the single source of truth that the daemon (through the C
//! ABI in [`ffi`]), the miners and the explorer share:
//!
//! * [`rules`]: the success condition, difficulty -> task, retargeting, decay.
//! * [`proof`]: the proof blob format (codecs; compression goes here).
//! * [`Seed`]: the perturbation's seed: all 256 bits of
//!   `cn_fast_hash(block hashing blob)`.
//! * [`verify`]: replays a proof and checks it. A proof is the frame count and
//!   the player's inputs on the frames that weren't poked: the game applies
//!   the random pad itself whenever a frame is poked, so pokes are never
//!   recorded (see [`proof::Proof`], [`Recorder`]).

pub mod ffi;
pub mod cache;
pub mod chain;
pub mod emulator;
pub mod game;
pub mod hold;
pub mod mining;
pub mod preverify;
pub mod proof;
pub mod proof_id;
pub mod replay;
pub mod rules;
pub mod seed;
pub mod statecodec;
pub mod statemix;

pub use emulator::{GAME_BUILD, default_rom_path, game_build_id, generator, init, init_default};
pub use game::{SM64MiningGame, SM64MiningSave};
pub use mining::{
    Recorder, Verdict, decode_proof_any, mine_step, new_mining_game, play_frame, played_from_proof, played_from_proof_at, proof_from_played,
    proof_from_played_at, seal_proof, seal_proof_at, seal_proof_state_at, verify, verify_pads, verify_proof,
};
pub use proof_id::proof_id;
pub use seed::{Seed, seed_from_hash};

pub use rules::{Params, Rules, active as rules, level8};
pub use sm64_binds;

/// What the active rules ask at `difficulty`.
pub fn params(difficulty: u64) -> Params {
    rules().params(difficulty)
}

