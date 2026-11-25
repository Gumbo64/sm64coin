//! Verifying one run (layer 4): where it may start and what it must do.
//!
//! `start_game` applies the start rules: the first block after the genesis
//! block and every block after a cycle-ending (Grand Star) block start from
//! power-on; any other block continues from its parent's end state, or
//! restarts from power-on at a price (its limit is the limit
//! [`crate::rules::Rules::restart_delay`] seconds earlier). `replay` and
//! `replay_state` then play the proof through the perturbation
//! ([`crate::game::perturbed_step`]) and accept it only if: it is within the
//! time limit, every non-poked frame has an input, the success condition
//! ([`crate::rules::Rules::is_success`]) first holds on exactly the last
//! frame, and no input is left over (state codecs: and the body is the
//! canonical encoding).
//!
//! Pure: the caller hands in the parent's end state; no cache, no globals.
//!
//! Read next: cache.rs, preverify.rs, then chain.rs (the verdict per block).

use crate::game::{Progress, SM64MiningGame, read_progress};
use crate::proof::{self, Proof, Start};
use crate::emulator::generator;
use crate::rules::{Params, active as rules, params};
use crate::seed::Seed;
use std::sync::Arc;
use anyhow::Result;
use sm64_binds::{GamePad, SaveState};

/// A block's end state: the game after its last frame, and its progress.
pub struct EndState {
    pub game: SaveState,
    pub progress: Progress,
    pub ends_cycle: bool,
    /// What the block's run added (bookkeeping: which star, key...).
    pub event: Option<crate::game::Event>,
}

/// The block's parent, as far as the chain rules care.
#[derive(Clone, Copy, Debug)]
pub enum Parent {
    /// The genesis block (built from config, never played): the first block
    /// starts from power-on.
    Genesis,
    /// Any other block, and how many seconds after it this block is
    /// timestamped.
    Block { id: [u8; 32], gap_seconds: u64 },
}

/// What a replay saw: the pad played on every frame and whether the frame
/// was poked (the pads of the other frames are the proof's inputs).
#[derive(Default, Clone, Debug)]
pub struct Played {
    pub pads: Vec<GamePad>,
    pub poked: Vec<bool>,
}

impl Played {
    /// The proof's inputs, whatever codec stored them (codec-free).
    pub fn wanted(&self) -> Vec<GamePad> {
        self.pads.iter().zip(&self.poked).filter(|(_, k)| !**k).map(|(p, _)| *p).collect()
    }
}

/// Where a block with this proof start and parent begins: a game, the
/// progress it already has and how many seconds earlier a power-on restart's
/// limit is taken ([`crate::Rules::restart_delay`]), or why the start isn't
/// allowed (None: parent state needed). With `checking` false (rebuilding an
/// accepted block) a restart needs no parent state.
pub(crate) fn start_game(start: Start, parent: &Parent, parent_state: Option<Arc<EndState>>, checking: bool) -> Result<Option<std::result::Result<(sm64_binds::SM64Game, Progress, u64), String>>> {
    let r = rules();
    let mut cost = 0;
    match (start, parent) {
        (Start::PowerOn, Parent::Genesis) => {}
        (Start::Parent, Parent::Genesis) => return Ok(Some(Err("the first block starts from power-on".into()))),
        (Start::PowerOn, Parent::Block { .. }) => {
            if checking {
                match &parent_state {
                    None => return Ok(None),
                    Some(s) if !s.ends_cycle => match r.restart_delay(&s.progress) {
                        Some(c) => cost = c,
                        None => return Ok(Some(Err("a power-on start needs a cycle-ending parent (restarts are off)".into()))),
                    },
                    Some(_) => {}
                }
            }
        }
        (Start::Parent, Parent::Block { .. }) => match &parent_state {
            None => return Ok(None),
            Some(s) if s.ends_cycle => return Ok(Some(Err("the parent ended the cycle: start from power-on".into()))),
            Some(s) => {
                let mut g = generator()?.create_game()?;
                g.load_state(&s.game)?;
                return Ok(Some(Ok((g, s.progress, 0))));
            }
        },
    }
    let g = generator()?.create_game()?;
    let p = read_progress(&g)?;
    Ok(Some(Ok((g, p, cost))))
}

/// Replays `pr` from `game` (progress `before`): Valid when the success
/// condition first holds on exactly the last frame with every input used.
/// `max_frames` None skips the time limit (rebuilding accepted blocks).
pub(crate) fn replay(game: sm64_binds::SM64Game, before: Progress, seed: &Seed, p: &Params, pr: &Proof, max_frames: Option<u32>, played: Option<&mut Played>) -> Result<std::result::Result<(SM64MiningGame, Progress), String>> {
    let r = rules();
    if pr.frames == 0 {
        return Ok(Err("empty proof".into()));
    }
    if let Some(m) = max_frames {
        if pr.frames > m {
            return Ok(Err(format!("{} frames exceeds the limit of {m}", pr.frames)));
        }
    }
    if pr.inputs.len() > pr.frames as usize {
        return Ok(Err("more inputs than frames".into()));
    }
    let mut g = SM64MiningGame::from_game(game, seed, p);
    let mut out = played;
    let mut next = 0usize;
    for f in 0..pr.frames as usize {
        let wanted = pr.inputs.get(next).copied().unwrap_or_default();
        let (pad, poked) = g.step(wanted)?;
        if let Some(o) = out.as_deref_mut() {
            o.pads.push(pad);
            o.poked.push(poked);
        }
        if !poked {
            if next >= pr.inputs.len() {
                return Ok(Err(format!("frame {f} needs an input but the proof has run out")));
            }
            next += 1;
        }
        let now = g.progress()?;
        if r.is_success(&before, &now) {
            if f + 1 != pr.frames as usize {
                return Ok(Err(format!("success at frame {} but the proof runs to {}", f + 1, pr.frames)));
            }
            if next != pr.inputs.len() {
                return Ok(Err(format!("{} inputs left over", pr.inputs.len() - next)));
            }
            return Ok(Ok((g, now)));
        }
    }
    Ok(Err("no progress was made (no new star, key or Grand Star)".into()))
}

/// [`replay`] for a state-coded proof: the inputs are decoded from `body`
/// frame by frame, in the state they are played in. Besides the usual
/// conditions the body must be exactly the canonical encoding of the decoded
/// inputs (re-encoded alongside), so a run has one valid encoding.
#[allow(clippy::too_many_arguments)]
pub(crate) fn replay_state(game: sm64_binds::SM64Game, before: Progress, seed: &Seed, p: &Params, frames: u32, codec: &dyn crate::statecodec::StateCodec, body: &[u8], max_frames: Option<u32>, played: Option<&mut Played>) -> Result<std::result::Result<(SM64MiningGame, Progress), String>> {
    let r = rules();
    if frames == 0 {
        return Ok(Err("empty proof".into()));
    }
    if let Some(m) = max_frames {
        if frames > m {
            return Ok(Err(format!("{frames} frames exceeds the limit of {m}")));
        }
    }
    // Never more inputs than frames: one decode per non-poked frame, at most `frames` frames.
    let mut g = SM64MiningGame::from_game(game, seed, p);
    let mut dec = codec.decoder(body);
    let mut out = played;
    for f in 0..frames as usize {
        let (pad, poked) = g.step_with(|ft| {
            Ok(crate::game::from_pad(dec.pad(ft)))
        })?;
        if poked {
            dec.poked();
        }
        if let Some(o) = out.as_deref_mut() {
            o.pads.push(pad);
            o.poked.push(poked);
        }
        let now = g.progress()?;
        if r.is_success(&before, &now) {
            if f + 1 != frames as usize {
                return Ok(Err(format!("success at frame {} but the proof runs to {frames}", f + 1)));
            }
            if dec.finish() != body {
                return Ok(Err("state-coded proof is not the canonical encoding".into()));
            }
            return Ok(Ok((g, now)));
        }
    }
    Ok(Err("no progress was made (no new star, key or Grand Star)".into()))
}

/// Replays a proof blob from `game` with no time limit (the rules that depend
/// on the timestamp are checked later, by [`verify_block`]).
pub(crate) fn replay_blob(game: sm64_binds::SM64Game, before: Progress, seed: &Seed, blob: &[u8], state_ok: bool) -> Result<std::result::Result<(SM64MiningGame, Progress, u32), String>> {
    let p = params(1);
    if state_ok {
        if let Ok((frames, _, cid, body)) = proof::split_blob(blob) {
            if let Some(c) = crate::statecodec::state_codec(cid) {
                return Ok(replay_state(game, before, seed, &p, frames, c, body, None, None)?.map(|(g, now)| (g, now, frames)));
            }
        }
    }
    let pr = match proof::decode_proof(blob, proof::MAX_FRAMES) {
        Ok(x) => x,
        Err(e) => return Ok(Err(format!("bad proof: {e}"))),
    };
    let frames = pr.frames;
    Ok(replay(game, before, seed, &p, &pr, None, None)?.map(|(g, now)| (g, now, frames)))
}
