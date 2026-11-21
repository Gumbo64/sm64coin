//! SM64Coin proof-of-play.
//!
//! A block is sealed by playing Super Mario 64 from power-on until the success
//! condition holds (by default: Mario has a star). While playing, the
//! controller is randomly perturbed ("poking the robot"): the RNG is seeded
//! from the block template and re-mixed with Mario's position and velocity
//! after every frame, so the perturbations cannot be precomputed, recycled from
//! an older run, or cherry-picked cheaply.
//!
//! This crate is the single source of truth that the daemon (through the C
//! ABI in [`ffi`]), the miners and the explorer share:
//!
//! * [`rules`]: the success condition, difficulty -> task, retargeting, decay.
//! * [`proof`]: the proof blob format (codecs; compression goes here).
//! * [`Seed`]: the perturbation's seed: all 256 bits of
//!   `cn_fast_hash(block hashing blob)`, fed in 32 bits at a time every
//!   `Params::reseed_frames` frames (xorshift32 only holds 32 bits; a single
//!   32-bit seed could be ground with the nonce to reuse a stockpiled run).
//! * [`verify`]: replays a proof and checks it.

use anyhow::{Result, anyhow};
use sha3::{Digest, Keccak256};
use sm64_binds::{GamePad, SM64Game, SM64GameGenerator};
use std::sync::OnceLock;

pub mod ffi;
pub mod proof;
pub mod rules;

pub use rules::{Params, Rules, active as rules, level8};
pub use sm64_binds;

/// What the active rules ask at `difficulty`.
pub fn params(difficulty: u64) -> Params {
    rules().params(difficulty)
}

/// The RNG seed for a block: the first four bytes (little endian) of
/// `cn_fast_hash(block hashing blob)`. xorshift32 cannot leave zero, so zero
/// becomes one. (The whole hash is used: see [`Seed`].)
pub fn seed_from_hash(hash: &[u8; 32]) -> u32 {
    Seed(*hash).word(0)
}

/// A block's perturbation seed: `cn_fast_hash(block hashing blob)`.
///
/// The xorshift32 state is reset to [`Seed::word`]`(k)` before frame
/// `k * reseed_frames`, so every bit of the hash shapes the run: a run
/// played for one block is useless for another beyond the first
/// `reseed_frames` frames, however the miner grinds nonces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seed(pub [u8; 32]);

impl Seed {
    /// Word `k`: 32 bits of the hash (cycling through its 8 words), mixed
    /// with `k` so later cycles differ. Never zero.
    pub fn word(&self, k: u32) -> u32 {
        let i = (k as usize % 8) * 4;
        let w = u32::from_le_bytes([self.0[i], self.0[i + 1], self.0[i + 2], self.0[i + 3]]);
        let w = w ^ (k / 8).wrapping_mul(0x9E37_79B9);
        if w == 0 { 1 } else { w }
    }

    pub fn from_hex(h: &str) -> Result<Seed> {
        let mut out = [0u8; 32];
        if h.len() != 64 {
            anyhow::bail!("seed hash must be 64 hex characters");
        }
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&h[2 * i..2 * i + 2], 16)?;
        }
        Ok(Seed(out))
    }
}

/// What a block id commits to: Keccak-256 of the decoded pads in raw form,
/// so a proof can be re-encoded (better compression) without changing any
/// block id. An undecodable blob gets a hash of its bytes behind a marker
/// (such a block fails verification anyway).
pub fn proof_id(blob: &[u8]) -> [u8; 32] {
    let mut h = Keccak256::new();
    // No valid proof is longer than the easiest task allows; decoding no
    // further keeps a tiny run-length blob from costing megabytes here.
    match proof::decode(blob, rules().max_frames_any() as usize) {
        Ok(pads) => {
            h.update(b"sm64pads");
            for p in &pads {
                h.update(proof::pad_bytes(p));
            }
        }
        Err(_) => {
            h.update(b"sm64bad!");
            h.update(blob);
        }
    }
    h.finalize().into()
}

static GENERATOR: OnceLock<SM64GameGenerator> = OnceLock::new();

/// Loads the ROM once per process. Later calls are no-ops.
pub fn init(rom: Vec<u8>) -> Result<&'static SM64GameGenerator> {
    if let Some(g) = GENERATOR.get() {
        return Ok(g);
    }
    let g = SM64GameGenerator::new(rom)?;
    let _ = GENERATOR.set(g);
    Ok(GENERATOR.get().unwrap())
}

/// The ROM path: `$SM64COIN_ROM`, else `baserom.us.z64` in the working
/// directory, else `~/.sm64coin/baserom.us.z64`.
pub fn default_rom_path() -> Option<String> {
    if let Ok(p) = std::env::var("SM64COIN_ROM") {
        return Some(p);
    }
    let mut cands = vec!["baserom.us.z64".to_string()];
    if let Ok(h) = std::env::var("HOME") {
        cands.push(format!("{h}/.sm64coin/baserom.us.z64"));
    }
    cands.into_iter().find(|p| std::path::Path::new(p).exists())
}

pub fn init_default() -> Result<&'static SM64GameGenerator> {
    if let Some(g) = GENERATOR.get() {
        return Ok(g);
    }
    let path = default_rom_path().ok_or_else(|| {
        anyhow!("no SM64 ROM: set SM64COIN_ROM or put baserom.us.z64 in the working directory")
    })?;
    init(std::fs::read(&path).map_err(|e| anyhow!("reading ROM {path}: {e}"))?)
}

pub fn generator() -> Result<&'static SM64GameGenerator> {
    init_default()
}

/// A fresh game, powered on, with the block's perturbation armed.
pub fn new_mining_game(seed: &Seed, p: &Params) -> Result<SM64Game> {
    let mut game = generator()?.create_game()?;
    game.set_rng_seed(seed.word(0))?;
    game.set_rng_config(p.rng)?;
    Ok(game)
}

/// Reseeds the perturbation if frame `frame` (0-based) starts a new
/// segment. Call before the frame's `rng_pad`.
pub fn reseed(game: &mut SM64Game, seed: &Seed, p: &Params, frame: usize) -> Result<()> {
    let r = p.reseed_frames as usize;
    if r != 0 && frame != 0 && frame % r == 0 {
        game.set_rng_seed(seed.word((frame / r) as u32))?;
    }
    Ok(())
}

/// Plays frame `frame` (0-based) the way a miner must: reseed when due, then
/// the controller goes through the perturbation. Returns the pad that was
/// actually played, which is what gets recorded in the block.
pub fn mine_step(game: &mut SM64Game, seed: &Seed, p: &Params, frame: usize, wanted: GamePad) -> Result<GamePad> {
    reseed(game, seed, p, frame)?;
    let played = game.rng_pad(wanted)?;
    game.step_game(played)?;
    Ok(played)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// The success condition first held on the last frame, `frames` long.
    Valid { frames: u32 },
    Invalid(String),
}

/// Replays the proof `blob` for a block with `seed` at (effective)
/// `difficulty`.
///
/// Valid when: it decodes, is no longer than the difficulty's time limit,
/// every pad is the one the perturbation allows on that frame (random frames
/// must hold the random pad), and the success condition first holds on
/// exactly the last frame (so no trailing input can be appended).
pub fn verify(seed: &Seed, difficulty: u64, blob: &[u8]) -> Result<Verdict> {
    let r = rules();
    let p = r.params(difficulty);
    let pads = match proof::decode(blob, p.max_frames as usize) {
        Ok(p) => p,
        Err(e) => return Ok(Verdict::Invalid(format!("bad proof: {e}"))),
    };
    verify_pads(seed, &p, &pads)
}

pub fn verify_pads(seed: &Seed, p: &Params, pads: &[GamePad]) -> Result<Verdict> {
    let r = rules();
    if pads.is_empty() {
        return Ok(Verdict::Invalid("empty proof".into()));
    }
    if pads.len() > p.max_frames as usize {
        return Ok(Verdict::Invalid(format!("{} frames exceeds the limit of {}", pads.len(), p.max_frames)));
    }
    let mut game = new_mining_game(seed, p)?;
    for (i, pad) in pads.iter().enumerate() {
        reseed(&mut game, seed, p, i)?;
        let allowed = game.rng_pad(*pad)?;
        if allowed != *pad {
            return Ok(Verdict::Invalid(format!("frame {i}: pad {pad:?} but the perturbation forces {allowed:?}")));
        }
        game.step_game(*pad)?;
        let st = game.get_game_state()?;
        if r.is_success(&game, &st) {
            if i + 1 != pads.len() {
                return Ok(Verdict::Invalid(format!(
                    "success at frame {} but the proof runs to {}",
                    i + 1,
                    pads.len()
                )));
            }
            return Ok(Verdict::Valid { frames: pads.len() as u32 });
        }
    }
    Ok(Verdict::Invalid("success condition never met".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_words_cover_the_hash() {
        let mut h = [0u8; 32];
        for (i, b) in h.iter_mut().enumerate() {
            *b = i as u8 + 1;
        }
        let s = Seed(h);
        let words: Vec<u32> = (0..8).map(|k| s.word(k)).collect();
        let mut d = words.clone();
        d.dedup();
        assert_eq!(d.len(), 8);
        assert_ne!(s.word(0), s.word(8), "later cycles differ");
        assert_eq!(Seed([0; 32]).word(0), 1);
        let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(Seed::from_hex(&hex).unwrap(), s);
    }

    #[test]
    fn seed_never_zero() {
        assert_eq!(seed_from_hash(&[0; 32]), 1);
        let mut h = [0u8; 32];
        h[0] = 7;
        assert_eq!(seed_from_hash(&h), 7);
    }

    #[test]
    fn proof_id_ignores_codec() {
        let pads = vec![GamePad::new(0, 0, 0); 40];
        let a = proof::encode_with(&proof::Raw, &pads);
        let b = proof::encode_with(&proof::RunLength, &pads);
        assert_ne!(a, b);
        assert_eq!(proof_id(&a), proof_id(&b));
        assert_ne!(proof_id(&a), proof_id(&[0, 1, 2]));
    }
}
