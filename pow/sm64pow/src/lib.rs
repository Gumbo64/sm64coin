//! SM64Coin proof-of-play: the consensus core.
//!
//! A block is sealed by playing Super Mario 64 on an emulated N64: its proof
//! is a run of controller inputs that, replayed from the parent block's end
//! state (or from power-on), makes the game's next progress point (a star, a
//! Bowser key, or the Grand Star). While the run plays the controller is
//! randomly "poked": pads replaced by random ones, drawn from the block's
//! seed and the game state ([`sm64coin_perturb`]), so a run only works for
//! the block it was played for. Every node verifies a block by replaying it.
//!
//! This crate is the single source of truth: the daemon calls it through the
//! C ABI in [`ffi`] (the C++ side has no rules of its own), and the miners,
//! SDK and explorer link it directly. Start at docs/CORE.md for the guided
//! tour. The modules, bottom layer first (each uses only the ones above it in
//! this list):
//!
//! 1. [`seed`], [`sign`], [`rules`](mod@rules), [`hold`], [`proof`]: pure, no game. v0.4:
//!    [`commit`] (the committed proof header), [`records`] (included proofs, fork-choice weight),
//!    [`payout`] (the outputs paying them). The seed; the
//!    miner key and block signature (signed proofs, v0.3); the
//!    consensus parameters as functions (time limit, retarget, restarts);
//!    the hold policy; the proof blob and its stateless codecs (0 raw,
//!    1 run-length, 2 xz).
//! 2. [`emulator`], [`game`]: the ROM and the game build; a game bundled
//!    with its perturbation ([`SM64MiningGame`]).
//! 3. [`statecodec`], [`proof_id`](mod@proof_id): the state-aware codecs (3-5, decoded
//!    while the game plays) and what a block id commits to.
//! 4. [`replay`]: verifying one run (where it may start, what it must do).
//! 5. [`cache`], [`preverify`], [`chain`]: blocks as a chain of end states:
//!    the cache, the parallel sync pre-pass, and the verdict per block;
//!    [`admit`]: which blocks earn a replay at all (relay policy, docs/DOS.md);
//!    [`equivocation`]: two signed runs for one template (relay policy);
//!    [`pof`]: publish-on-find's displacement window and the freshness bound.
//! 6. [`ffi`]: the C ABI the daemon calls (`sm64pow.h`).
//!
//! Beside the consensus path: [`mining`] (building and sealing proofs, the
//! standalone `verify`), and the research tooling at the bottom of
//! [`statecodec`].

// 1: pure
pub mod commit;
pub mod hold;
pub mod payout;
pub mod records;
pub mod proof;
pub mod template;
pub mod reorg;
pub mod rules;
pub mod seed;
pub mod sign;
// 2: the game
pub mod emulator;
pub mod game;
// 3: codecs that need the game, block ids
pub mod proof_id;
pub mod statecodec;
pub mod statemix;
// 4-5: verification
pub mod admit;
pub mod cache;
pub mod chain;
pub mod equivocation;
pub mod pof;
pub mod preverify;
pub mod queue;
pub mod replay;
// 6: the daemon's C ABI
pub mod ffi;
// beside the consensus path
pub mod mining;

pub use emulator::{GAME_BUILD, default_rom_path, game_build_id, generator, init, init_default};
pub use game::{SM64MiningGame, SM64MiningSave};
pub use mining::{
    Recorder, Verdict, decode_proof_any, mine_step, new_mining_game, play_frame, played_from_proof, played_from_proof_at, proof_from_played,
    proof_from_played_at, seal_proof, seal_proof_at, seal_proof_state_at, verify, verify_pads, verify_proof,
};
pub use proof_id::proof_id;
pub use seed::{Seed, seed_from_hash};

pub use rules::{Params, Rules, active as rules, level8, params};
pub use sm64_binds;

