//! State-aware proof codecs (layer 3): the inputs are coded given the game
//! state they are played in, so decoding happens frame by frame during the
//! replay.
//!
//! The codecs themselves are the pure `sm64coin-statecodec` crate
//! (pow/statecodec: a binary range coder and adaptive context-mixing models,
//! integer arithmetic only, shared with the browser); this module re-exports
//! it and adds the replay that feeds it game states: [`encode_proof_state`]
//! (sealing) and [`decode_proof_state`] (tools; the verifier decodes inside
//! `replay::replay_state`). A state-coded proof has exactly one valid
//! encoding (the verifier re-encodes and compares), and the block id commits
//! to its bytes ([`state_proof_id`]).
//!
//! On by default (cargo feature `state-codec`, the state-codec hard fork);
//! built without it, nodes reject codecs 3-5 like any unknown id. The bottom
//! of the file is research tooling ([`Run`], [`state_after`]), not consensus.
//!
//! Read next: proof_id.rs.

use crate::emulator::generator;
use crate::game::mining_start;
use crate::proof::{Proof, Start};
use crate::rules::{Params, params};
use crate::seed::Seed;
use anyhow::Result;
#[allow(hidden_glob_reexports)]
use sm64_binds::{GamePad, SaveState};
// The codec crate's pad is `Pad` here; `GamePad` stays the game's (native) pad.
pub use sm64coin_statecodec::*;
use crate::game::{from_pad, to_pad};

/// Whether blocks may use state codecs: the hard fork switch. On unless
/// built without the (default) `state-codec` feature.
pub const fn state_codec_enabled() -> bool {
    cfg!(feature = "state-codec")
}

/// The proof blob (`varint(frames) || start || id || body`) of `pr` in a
/// state codec: replays the run from `parent` (the parent's end state, for a
/// parent start) to code each input in its state.
pub fn encode_proof_state(c: &dyn StateCodec, parent: Option<&SaveState>, seed: &Seed, p: &Params, pr: &Proof) -> Result<Vec<u8>> {
    let (mut g, _) = mining_start(pr.start, parent, seed, p)?;
    let mut enc = c.encoder();
    let mut next = 0;
    for f in 0..pr.frames {
        let wanted = pr.inputs.get(next).copied();
        let (_, poked) = g.step_with(|ft| {
            let w = wanted.ok_or_else(|| anyhow::anyhow!("frame {f} needs an input but the proof has run out"))?;
            enc.pad(ft, to_pad(w));
            Ok(w)
        })?;
        if poked {
            enc.poked();
        } else {
            next += 1;
        }
    }
    anyhow::ensure!(next == pr.inputs.len(), "{} inputs left over", pr.inputs.len() - next);
    let mut out = Vec::new();
    crate::proof::put_varint(&mut out, pr.frames as u64);
    out.push(pr.start as u8);
    out.push(c.id());
    out.extend(enc.finish());
    Ok(out)
}

/// Decodes a state-coded proof blob to its codec-free [`Proof`] by replaying
/// the run from `parent` (the parent's end state, for a parent start): the
/// inputs are decoded frame by frame in the states they are played in, and
/// the body must be the canonical encoding. Does not check that the run
/// succeeds. For tools (SDK, explorers); the verifier does this inside
/// [`crate::chain`]'s replay.
pub fn decode_proof_state(blob: &[u8], parent: Option<&SaveState>, seed: &Seed, p: &Params) -> Result<Proof> {
    let (frames, start, id, body) = crate::proof::split_blob(blob)?;
    let c = state_codec(id).ok_or_else(|| anyhow::anyhow!("unknown proof codec {id}"))?;
    let (mut g, _) = mining_start(start, parent, seed, p)?;
    let mut dec = c.decoder(body);
    let mut inputs = Vec::new();
    for _ in 0..frames {
        let (pad, poked) = g.step_with(|ft| Ok(from_pad(dec.pad(ft))))?;
        if poked {
            dec.poked();
        } else {
            inputs.push(pad);
        }
    }
    anyhow::ensure!(dec.finish() == body, "state-coded proof is not the canonical encoding");
    Ok(Proof { frames, start, inputs })
}

/// proof_id of a state-coded blob: it commits to the encoded bytes (decoding
/// needs the parent's state), which are canonical.
pub fn state_proof_id(blob: &[u8]) -> [u8; 32] {
    use sha3::{Digest, Keccak256};
    let mut h = Keccak256::new();
    h.update(b"sm64state");
    h.update(blob);
    h.finalize().into()
}

// ---- Research tooling (not consensus): dump a run's states for codec work ----

/// A run with everything needed to replay it: where it starts, the parent's
/// played pads (to rebuild the parent's end state for a parent start), the
/// block's seed, and its proof.
#[derive(Clone, Debug)]
pub struct Run {
    pub seed: Seed,
    pub parent_played: Vec<GamePad>,
    pub proof: Proof,
}

/// The game state after playing `played` from power-on, unperturbed.
pub fn state_after(played: &[GamePad]) -> Result<SaveState> {
    let mut g = generator()?.create_game()?;
    for p in played {
        g.step_game(*p)?;
    }
    Ok(g.save_state())
}

impl Run {
    pub fn parent_state(&self) -> Result<Option<SaveState>> {
        Ok(match self.proof.start {
            Start::PowerOn => None,
            Start::Parent => Some(state_after(&self.parent_played)?),
        })
    }

    /// Replays the run, listing for every frame the features before it, the
    /// pad played and whether it was a poke.
    pub fn dump_rows(&self) -> Result<Vec<(Features, GamePad, bool)>> {
        let ps = self.parent_state()?;
        let (mut g, _) = mining_start(self.proof.start, ps.as_ref(), &self.seed, &params(1))?;
        let mut rows = Vec::new();
        let mut next = 0;
        for _ in 0..self.proof.frames {
            let wanted = self.proof.inputs.get(next).copied().unwrap_or_default();
            let mut feat = Features::default();
            let (pad, poked) = g.step_with(|f| {
                feat = *f;
                Ok(wanted)
            })?;
            if !poked {
                next += 1;
            }
            rows.push((feat, pad, poked));
        }
        Ok(rows)
    }
}
