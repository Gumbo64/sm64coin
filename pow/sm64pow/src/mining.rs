//! The miner's side (beside the consensus path): playing perturbed frames,
//! building proofs from played pads, sealing them in the smallest codec, and
//! checking a run standalone ([`verify`], as the first block of a cycle).
//!
//! Miners keep the pads they played on every frame; [`seal_proof_at`] turns
//! that into a proof blob by replaying it and dropping the poked frames.
//! Used by pow/miner, the SDK and the web miner; nodes don't call it.

use crate::game::{SM64MiningGame, mining_start};
use crate::rules::Params;
use crate::seed::Seed;
use crate::{chain, proof, statecodec};
use anyhow::Result;
use sm64_binds::GamePad;

/// A fresh game, powered on, with the block's perturbation armed.
pub fn new_mining_game(seed: &Seed, p: &Params) -> Result<SM64MiningGame> {
    SM64MiningGame::new(seed, p)
}

/// Plays one frame the way a miner must: the controller goes through the
/// perturbation, which may replace it with a random pad (a poke). Returns the
/// pad actually played and whether the frame was poked.
pub fn play_frame(game: &mut SM64MiningGame, wanted: GamePad) -> Result<(GamePad, bool)> {
    game.step(wanted)
}

/// Like [`play_frame`], returning only the pad played.
pub fn mine_step(game: &mut SM64MiningGame, wanted: GamePad) -> Result<GamePad> {
    Ok(game.step(wanted)?.0)
}

/// Builds a proof while playing: records the player's input on every frame
/// that wasn't poked; poked frames aren't recorded.
#[derive(Default, Clone, Debug)]
pub struct Recorder {
    pub proof: proof::Proof,
}

impl Recorder {
    pub fn step(&mut self, game: &mut SM64MiningGame, wanted: GamePad) -> Result<GamePad> {
        let (played, poked) = play_frame(game, wanted)?;
        if !poked {
            self.proof.inputs.push(wanted);
        }
        self.proof.frames += 1;
        Ok(played)
    }
}

/// A fresh game for a run with `start`: power-on, or loaded from the
/// parent's end state `parent` (a native save state).
fn start_game(start: proof::Start, parent: Option<&sm64_binds::SaveState>, seed: &Seed, p: &Params) -> Result<SM64MiningGame> {
    Ok(mining_start(start, parent, seed, p)?.0)
}

/// The proof for a run given as the pads played on every frame (what the
/// search-based miners keep), starting at `start` (with the parent's end
/// state for [`proof::Start::Parent`]): replays it and drops the poked frames.
pub fn proof_from_played_at(start: proof::Start, parent: Option<&sm64_binds::SaveState>, seed: &Seed, p: &Params, played: &[GamePad]) -> Result<proof::Proof> {
    let mut game = start_game(start, parent, seed, p)?;
    let mut rec = Recorder::default();
    rec.proof.start = start;
    for pad in played {
        let got = rec.step(&mut game, *pad)?;
        anyhow::ensure!(got == *pad, "frame {}: recorded {pad:?} but the perturbation played {got:?}", rec.proof.frames - 1);
    }
    Ok(rec.proof)
}

/// [`proof_from_played_at`] for a run from power-on.
pub fn proof_from_played(seed: &Seed, p: &Params, played: &[GamePad]) -> Result<proof::Proof> {
    proof_from_played_at(proof::Start::PowerOn, None, seed, p, played)
}

/// The pads played on every frame of a proof's run (pokes regenerated),
/// given the parent's end state for a [`proof::Start::Parent`] proof.
pub fn played_from_proof_at(parent: Option<&sm64_binds::SaveState>, seed: &Seed, p: &Params, pr: &proof::Proof) -> Result<Vec<GamePad>> {
    let mut game = start_game(pr.start, parent, seed, p)?;
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

/// [`played_from_proof_at`] for a proof from power-on.
pub fn played_from_proof(seed: &Seed, p: &Params, pr: &proof::Proof) -> Result<Vec<GamePad>> {
    played_from_proof_at(None, seed, p, pr)
}

/// The proof blob for a run given as the pads played on every frame.
pub fn seal_proof_at(start: proof::Start, parent: Option<&sm64_binds::SaveState>, seed: &Seed, p: &Params, played: &[GamePad]) -> Result<Vec<u8>> {
    let pr = proof_from_played_at(start, parent, seed, p, played)?;
    let best = proof::encode_proof(&pr);
    if statecodec::state_codec_enabled() {
        // Hard fork switch: a state codec may win.
        let mut best = best;
        for c in statecodec::SEAL_CODECS {
            let b = statecodec::encode_proof_state(*c, parent, seed, p, &pr)?;
            if b.len() < best.len() {
                best = b;
            }
        }
        return Ok(best);
    }
    Ok(best)
}

/// A proof blob in any codec to its codec-free [`proof::Proof`]; state-aware
/// codecs are decoded by replaying the run from `parent` (see
/// [`statecodec::decode_proof_state`]).
pub fn decode_proof_any(blob: &[u8], parent: Option<&sm64_binds::SaveState>, seed: &Seed, p: &Params) -> Result<proof::Proof> {
    if statecodec::state_codec_enabled() && proof::split_blob(blob).is_ok_and(|b| statecodec::state_codec(b.2).is_some()) {
        return statecodec::decode_proof_state(blob, parent, seed, p);
    }
    proof::decode_proof(blob, proof::MAX_FRAMES)
}

/// The proof blob in a state-aware codec (see [`statecodec`]): the same run,
/// coded given the game states it passes through.
pub fn seal_proof_state_at(c: &dyn statecodec::StateCodec, start: proof::Start, parent: Option<&sm64_binds::SaveState>, seed: &Seed, p: &Params, played: &[GamePad]) -> Result<Vec<u8>> {
    statecodec::encode_proof_state(c, parent, seed, p, &proof_from_played_at(start, parent, seed, p, played)?)
}

/// [`seal_proof_at`] for a run from power-on.
pub fn seal_proof(seed: &Seed, p: &Params, played: &[GamePad]) -> Result<Vec<u8>> {
    seal_proof_at(proof::Start::PowerOn, None, seed, p, played)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// The success condition first held on the last frame, `frames` long.
    Valid { frames: u32 },
    Invalid(String),
}

impl From<chain::BlockVerdict> for Verdict {
    fn from(v: chain::BlockVerdict) -> Verdict {
        match v {
            chain::BlockVerdict::Valid { frames, .. } => Verdict::Valid { frames },
            chain::BlockVerdict::Invalid(w) => Verdict::Invalid(w),
            chain::BlockVerdict::NeedParent => Verdict::Invalid("needs the parent's end state".into()),
        }
    }
}

/// Verifies a run from power-on standalone (as the first block of a cycle):
/// within the difficulty's time limit, every poked frame plays its random
/// pad, and the success condition first holds on exactly the last frame. For
/// blocks in a chain (which start from their parent's end state) see
/// [`chain::verify_block`].
pub fn verify(seed: &Seed, difficulty: u64, blob: &[u8]) -> Result<Verdict> {
    Ok(chain::verify_block(&[0xff; 32], chain::Parent::Genesis, seed, difficulty, blob)?.into())
}

/// [`verify`] for a decoded proof.
pub fn verify_proof(seed: &Seed, p: &Params, pr: &proof::Proof) -> Result<Verdict> {
    if pr.frames > p.max_frames {
        return Ok(Verdict::Invalid(format!("{} frames exceeds the limit of {}", pr.frames, p.max_frames)));
    }
    let blob = proof::encode_proof_with(&proof::Raw, pr);
    Ok(chain::rebuild_block(&[0xff; 32], chain::Parent::Genesis, seed, &blob)?.into())
}

/// Verifies a run from power-on given as the pads played on every frame.
pub fn verify_pads(seed: &Seed, p: &Params, played: &[GamePad]) -> Result<Verdict> {
    match proof_from_played(seed, p, played) {
        Ok(pr) => verify_proof(seed, p, &pr),
        Err(e) => Ok(Verdict::Invalid(e.to_string())),
    }
}

/// Signs a sealed proof `blob` for the template with seed `seed` (the
/// template's `sm64_seed_hash`, whose coinbase extra nonce committed
/// `key.extra_nonce()`): the block's `sm64_sig` (sign.rs; v0.3).
pub fn sign_proof(key: &crate::sign::Keypair, seed: &[u8; 32], blob: &[u8]) -> Result<[u8; 64]> {
    let frames = proof::split_blob(blob)?.0;
    Ok(key.sign_run(seed, &crate::proof_id::proof_id(blob), frames))
}

/// The submit-ready block: the template blob (ending with the empty proof,
/// a 0 byte, and the empty signature, 64 zero bytes) with `proof` and its
/// signature in their place, and the header timestamp set to `timestamp`
/// when given (the seed excludes it, so the signature stays valid).
pub fn seal_block(template: &[u8], proof_blob: &[u8], sig: &[u8; 64], timestamp: Option<u64>) -> Result<Vec<u8>> {
    let n = template.len();
    if n < 65 || template[n - 65..].iter().any(|&b| b != 0) {
        anyhow::bail!("template does not end with an empty proof and signature; is this an SM64Coin v0.3 daemon?");
    }
    let mut blob = template[..n - 65].to_vec();
    if let Some(ts) = timestamp {
        // Header: varint major, varint minor, varint timestamp, ...
        let vl = |b: &[u8]| b.iter().position(|x| x & 0x80 == 0).map(|i| i + 1).ok_or_else(|| anyhow::anyhow!("truncated header"));
        let a = vl(&blob)?;
        let b = a + vl(&blob[a..])?;
        let c = b + vl(&blob[b..])?;
        let mut out = blob[..b].to_vec();
        put_varint(&mut out, ts);
        out.extend_from_slice(&blob[c..]);
        blob = out;
    }
    put_varint(&mut blob, proof_blob.len() as u64);
    blob.extend_from_slice(proof_blob);
    blob.extend_from_slice(sig);
    Ok(blob)
}

fn put_varint(out: &mut Vec<u8>, mut n: u64) {
    while n >= 0x80 {
        out.push((n as u8) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}
