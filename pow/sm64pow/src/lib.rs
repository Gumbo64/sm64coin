//! SM64Coin proof-of-play.
//!
//! A block is sealed by playing Super Mario 64 from power-on until the success
//! condition holds (by default: Mario has a star). While playing, the
//! controller is randomly perturbed ("poking the robot"), outside the game
//! ([`perturb`]): a 256-bit generator seeded from the whole block hash, mixed
//! with a digest of the game state ([`game`]) after every frame the game
//! simulates, and scheduled on the game's own clock (so pausing neither spends
//! nor rerolls pokes). The game build is stock; [`MiningGame`] bundles it with
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

use anyhow::{Result, anyhow};
use sha3::{Digest, Keccak256};
use sm64_binds::{GamePad, SM64GameGenerator};
use std::sync::OnceLock;

pub mod ffi;
pub mod game;
pub mod proof;
pub mod rules;

pub use game::{MiningGame, MiningSave};

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

    /// The 8 little-endian 32-bit words the game's generator is seeded with.
    pub fn words(&self) -> [u32; 8] {
        let mut w = [0u32; 8];
        for (k, x) in w.iter_mut().enumerate() {
            *x = u32::from_le_bytes([self.0[4 * k], self.0[4 * k + 1], self.0[4 * k + 2], self.0[4 * k + 3]]);
        }
        w
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

/// What a block id commits to: Keccak-256 of the decoded proof (frame count
/// and inputs), so a proof can be re-encoded with another codec without
/// changing any block id. An undecodable blob gets a hash of its bytes behind
/// a marker (such a block fails verification anyway).
pub fn proof_id(blob: &[u8]) -> [u8; 32] {
    let mut h = Keccak256::new();
    // No valid proof is longer than the easiest task allows; decoding no
    // further keeps a tiny compressed blob from costing megabytes here.
    match proof::decode_proof(blob, rules().max_frames_any() as usize) {
        Ok(pr) => {
            h.update(b"sm64run1");
            h.update(pr.frames.to_le_bytes());
            for p in &pr.inputs {
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
pub fn new_mining_game(seed: &Seed, p: &Params) -> Result<MiningGame> {
    MiningGame::new(seed, p)
}

/// Plays one frame the way a miner must: the controller goes through the
/// perturbation, which may replace it with a random pad (a poke). Returns the
/// pad actually played and whether the frame was poked.
pub fn play_frame(game: &mut MiningGame, wanted: GamePad) -> Result<(GamePad, bool)> {
    game.step(wanted)
}

/// Like [`play_frame`], returning only the pad played.
pub fn mine_step(game: &mut MiningGame, wanted: GamePad) -> Result<GamePad> {
    Ok(game.step(wanted)?.0)
}

/// Builds a proof while playing: records the player's input on every frame
/// that wasn't poked; poked frames aren't recorded.
#[derive(Default, Clone, Debug)]
pub struct Recorder {
    pub proof: proof::Proof,
}

impl Recorder {
    pub fn step(&mut self, game: &mut MiningGame, wanted: GamePad) -> Result<GamePad> {
        let (played, poked) = play_frame(game, wanted)?;
        if !poked {
            self.proof.inputs.push(wanted);
        }
        self.proof.frames += 1;
        Ok(played)
    }
}

/// The proof for a run given as the pads played on every frame (what the
/// search-based miners keep): replays it and drops the poked frames.
pub fn proof_from_played(seed: &Seed, p: &Params, played: &[GamePad]) -> Result<proof::Proof> {
    let mut game = new_mining_game(seed, p)?;
    let mut rec = Recorder::default();
    for pad in played {
        let got = rec.step(&mut game, *pad)?;
        anyhow::ensure!(got == *pad, "frame {}: recorded {pad:?} but the perturbation played {got:?}", rec.proof.frames - 1);
    }
    Ok(rec.proof)
}

/// The pads played on every frame of a proof's run (pokes regenerated).
pub fn played_from_proof(seed: &Seed, p: &Params, pr: &proof::Proof) -> Result<Vec<GamePad>> {
    let mut game = new_mining_game(seed, p)?;
    let mut out = Vec::with_capacity(pr.frames as usize);
    let mut next = 0;
    for f in 0..pr.frames as usize {
        let wanted = pr.inputs.get(next).copied().unwrap_or_default();
        let (played, poked) = play_frame(&mut game, wanted)?;
        if !poked {
            anyhow::ensure!(next < pr.inputs.len(), "frame {f} needs an input but the proof has run out");
            next += 1;
        }
        out.push(played);
    }
    Ok(out)
}

/// The proof blob for a run given as the pads played on every frame.
pub fn seal_proof(seed: &Seed, p: &Params, played: &[GamePad]) -> Result<Vec<u8>> {
    Ok(proof::encode_proof(&proof_from_played(seed, p, played)?))
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
/// every pad is the one the perturbation allows on that frame (poked frames
/// must hold the random pad), and the success condition first holds on
/// exactly the last frame (so no trailing input can be appended).
pub fn verify(seed: &Seed, difficulty: u64, blob: &[u8]) -> Result<Verdict> {
    let r = rules();
    let p = r.params(difficulty);
    let pr = match proof::decode_proof(blob, p.max_frames as usize) {
        Ok(x) => x,
        Err(e) => return Ok(Verdict::Invalid(format!("bad proof: {e}"))),
    };
    verify_proof(seed, &p, &pr)
}

/// Replays a proof: each frame the next input goes through the perturbation;
/// on a poked frame the game plays its random pad and the input isn't used.
/// Valid when the success condition first holds on the last frame, every
/// input was used, and the run is within the time limit.
pub fn verify_proof(seed: &Seed, p: &Params, pr: &proof::Proof) -> Result<Verdict> {
    let r = rules();
    if pr.frames == 0 {
        return Ok(Verdict::Invalid("empty proof".into()));
    }
    if pr.frames > p.max_frames {
        return Ok(Verdict::Invalid(format!("{} frames exceeds the limit of {}", pr.frames, p.max_frames)));
    }
    if pr.inputs.len() > pr.frames as usize {
        return Ok(Verdict::Invalid("more inputs than frames".into()));
    }
    let mut game = new_mining_game(seed, p)?;
    let mut next = 0usize;
    for f in 0..pr.frames as usize {
        let wanted = pr.inputs.get(next).copied().unwrap_or_default();
        let (_, poked) = play_frame(&mut game, wanted)?;
        if !poked {
            if next >= pr.inputs.len() {
                return Ok(Verdict::Invalid(format!("frame {f} needs an input but the proof has run out")));
            }
            next += 1;
        }
        let st = game.state()?;
        if r.is_success(&game.game, &st) {
            if f + 1 != pr.frames as usize {
                return Ok(Verdict::Invalid(format!("success at frame {} but the proof runs to {}", f + 1, pr.frames)));
            }
            if next != pr.inputs.len() {
                return Ok(Verdict::Invalid(format!("{} inputs left over", pr.inputs.len() - next)));
            }
            return Ok(Verdict::Valid { frames: pr.frames });
        }
    }
    Ok(Verdict::Invalid("success condition never met".into()))
}

/// Verifies a run given as the pads played on every frame.
pub fn verify_pads(seed: &Seed, p: &Params, played: &[GamePad]) -> Result<Verdict> {
    match proof_from_played(seed, p, played) {
        Ok(pr) => verify_proof(seed, p, &pr),
        Err(e) => Ok(Verdict::Invalid(e.to_string())),
    }
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
        let pr = proof::Proof { frames: 50, inputs: vec![GamePad::new(0, 0, 0); 40] };
        let a = proof::encode_proof_with(&proof::Raw, &pr);
        let b = proof::encode_proof_with(&proof::RunLength, &pr);
        let c = proof::encode_proof_with(&proof::Xz, &pr);
        assert_ne!(a, b);
        assert_eq!(proof_id(&a), proof_id(&b));
        assert_eq!(proof_id(&a), proof_id(&c));
        assert_ne!(proof_id(&a), proof_id(&[0, 1, 2]));
        let other = proof::Proof { frames: 51, ..pr.clone() };
        assert_ne!(proof_id(&a), proof_id(&proof::encode_proof(&other)), "frame count is committed");
    }
}
