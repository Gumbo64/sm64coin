//! Blocks as a chain of game states: each block's run starts where its parent's
//! ended (or from power-on, see [`proof::Start`]), so verifying a block needs
//! its parent's end state.
//!
//! End states are cached here by block id (a few, they are megabytes each).
//! When a parent's state isn't cached, [`verify_block`] answers
//! [`BlockVerdict::NeedParent`]; the caller (the daemon) then walks back to a
//! block it can start from (one with a cached parent, a power-on start, or the
//! genesis block as parent), and replays forward with [`rebuild_block`].
//! Replay is fast (a whole cycle is seconds), so nothing is stored on disk.

use crate::cache::{REPLAYS, alias, known_summary, played_cached, remember, remember_played, run_key};
use crate::proof::{self, Start};
use crate::game::Progress;
use crate::replay::{replay, replay_state, start_game};
use crate::{Seed, params, rules};
use anyhow::Result;
use sm64_binds::GamePad;

pub use crate::cache::{cached, end_state_gz, info, replays, set_state_dir};
pub use crate::game::mining_start;
pub use crate::hold::hold_until;
pub use crate::preverify::{BatchBlock, preverify};
pub use crate::replay::{EndState, Parent, Played};

#[derive(Debug, Clone, PartialEq)]
pub enum BlockVerdict {
    Valid { frames: u32, progress: Progress, ends_cycle: bool },
    Invalid(String),
    /// The parent's end state isn't cached: replay its ancestors first.
    NeedParent,
}

fn check(id: &[u8; 32], parent: &Parent, seed: &Seed, difficulty: Option<u64>, blob: &[u8], played: Option<&mut Played>, state_ok: bool) -> Result<BlockVerdict> {
    let p = params(difficulty.unwrap_or(1));
    let elapsed = match parent {
        Parent::Block { gap_seconds, .. } => *gap_seconds,
        // The genesis block is built from config, long before the first block: the limit
        // has long grown to its cap (with the start offset, 0 would leave it at ~47 s).
        Parent::Genesis => u64::MAX / 2,
    };
    let mut limit = match difficulty {
        Some(d) => rules().max_frames(d, elapsed),
        None => proof::MAX_FRAMES as u32,
    };
    // State-aware codecs are decoded during the replay.
    let state = match proof::split_blob(blob) {
        Ok((frames, start, cid, body)) if state_ok => crate::statecodec::state_codec(cid).map(|c| (frames, start, c, body)),
        _ => None,
    };
    let (start, pr) = match &state {
        Some((_, start, _, _)) => (*start, None),
        None => match proof::decode_proof(blob, limit as usize) {
            Ok(x) => (x.start, Some(x)),
            Err(e) => return Ok(BlockVerdict::Invalid(format!("bad proof: {e}"))),
        },
    };
    // The same run already verified (a re-stamped copy of a block, or the
    // parallel pre-pass, see [`preverify`]): only the rules that depend on the
    // timestamp and the parent (the start, the restart delay, the time limit)
    // are checked again; the replay itself is not repeated. Not for `played`
    // (that wants the pads).
    if played.is_none() {
        let key = run_key(parent, start, seed, blob);
        if let Some(sum) = known_summary(&key) {
            if sum.start == start {
                let mut delay = 0;
                match (start, parent) {
                    (Start::PowerOn, Parent::Genesis) => {}
                    (Start::Parent, Parent::Genesis) => return Ok(BlockVerdict::Invalid("the first block starts from power-on".into())),
                    (Start::PowerOn, Parent::Block { id: pid, .. }) => {
                        if difficulty.is_some() {
                            match info(pid) {
                                None => return Ok(BlockVerdict::NeedParent),
                                Some((pp, false, _)) => match rules().restart_delay(&pp) {
                                    Some(c) => delay = c,
                                    None => return Ok(BlockVerdict::Invalid("a power-on start needs a cycle-ending parent (restarts are off)".into())),
                                },
                                Some(_) => {}
                            }
                        }
                    }
                    (Start::Parent, Parent::Block { id: pid, .. }) => match info(pid) {
                        None => return Ok(BlockVerdict::NeedParent),
                        Some((_, true, _)) => return Ok(BlockVerdict::Invalid("the parent ended the cycle: start from power-on".into())),
                        Some(_) => {}
                    },
                }
                if let (Some(d), true) = (difficulty, delay > 0) {
                    if elapsed < delay {
                        return Ok(BlockVerdict::Invalid(format!("a restart is allowed {delay} s after the parent, this block is {elapsed} s after it")));
                    }
                    limit = rules().max_frames(d, elapsed - delay);
                }
                if difficulty.is_some() && sum.frames > limit {
                    return Ok(BlockVerdict::Invalid(format!("{} frames exceeds the limit of {limit}", sum.frames)));
                }
                alias(*id, key);
                return Ok(BlockVerdict::Valid { frames: sum.frames, progress: sum.progress, ends_cycle: sum.ends_cycle });
            }
        }
    }
    let (game, before) = match start_game(start, parent, difficulty.is_some())? {
        None => return Ok(BlockVerdict::NeedParent),
        Some(Err(why)) => return Ok(BlockVerdict::Invalid(why)),
        Some(Ok((g, p, delay))) => {
            if let (Some(d), true) = (difficulty, delay > 0) {
                if elapsed < delay {
                    return Ok(BlockVerdict::Invalid(format!("a restart is allowed {delay} s after the parent, this block is {elapsed} s after it")));
                }
                limit = rules().max_frames(d, elapsed - delay);
                if let Some(pr) = &pr {
                    if pr.frames > limit {
                        return Ok(BlockVerdict::Invalid(format!("{} frames exceeds a restart's limit of {limit} (the limit {delay} s earlier)", pr.frames)));
                    }
                }
            }
            (g, p)
        }
    };
    let key = run_key(parent, start, seed, blob);
    REPLAYS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (frames, result) = match (&state, &pr) {
        (Some((frames, _, c, body)), _) => (*frames, replay_state(game, before, seed, &p, *frames, *c, body, difficulty.map(|_| limit), played)?),
        (None, Some(pr)) => (pr.frames, replay(game, before, seed, &p, pr, difficulty.map(|_| limit), played)?),
        _ => unreachable!(),
    };
    match result {
        Err(why) => Ok(BlockVerdict::Invalid(why)),
        Ok((g, now)) => {
            let ends_cycle = rules().ends_cycle(&now);
            let event = crate::game::event(&before, &now);
            remember(*id, key, EndState { game: g.game.save_state(), progress: now, ends_cycle, event }, frames, start, difficulty.is_some());
            Ok(BlockVerdict::Valid { frames, progress: now, ends_cycle })
        }
    }
}

/// Verifies block `id` (its proof `blob`, seed and retarget difficulty: the
/// time limit also depends on the parent gap, see [`crate::Rules::max_frames`])
/// on top of `parent`, and caches its end state when valid.
pub fn verify_block(id: &[u8; 32], parent: Parent, seed: &Seed, difficulty: u64, blob: &[u8]) -> Result<BlockVerdict> {
    verify_block_opts(id, parent, seed, difficulty, blob, crate::statecodec::state_codec_enabled())
}

/// [`verify_block`] with the state-codec switch given (see
/// [`crate::statecodec::state_codec_enabled`]; tests turn it on).
pub fn verify_block_opts(id: &[u8; 32], parent: Parent, seed: &Seed, difficulty: u64, blob: &[u8], state_ok: bool) -> Result<BlockVerdict> {
    check(id, &parent, seed, Some(difficulty), blob, None, state_ok)
}

/// Replays an already-accepted block to cache its end state (no time limit:
/// its difficulty was checked when it was accepted).
pub fn rebuild_block(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8]) -> Result<BlockVerdict> {
    rebuild_block_opts(id, parent, seed, blob, crate::statecodec::state_codec_enabled())
}

pub fn rebuild_block_opts(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8], state_ok: bool) -> Result<BlockVerdict> {
    check(id, &parent, seed, None, blob, None, state_ok)
}

/// The pads a block's run played on every frame (pokes included), e.g. for
/// replays and `.m64` export. Needs the parent's end state, like verifying.
pub fn played_block(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8]) -> Result<std::result::Result<Vec<GamePad>, BlockVerdict>> {
    played_block_opts(id, parent, seed, blob, crate::statecodec::state_codec_enabled())
}

pub fn played_block_opts(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8], state_ok: bool) -> Result<std::result::Result<Vec<GamePad>, BlockVerdict>> {
    Ok(played_wanted_block_opts(id, parent, seed, blob, state_ok)?.map(|p| p.pads))
}

pub fn played_wanted_block(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8]) -> Result<std::result::Result<Played, BlockVerdict>> {
    played_wanted_block_opts(id, parent, seed, blob, crate::statecodec::state_codec_enabled())
}

pub fn played_wanted_block_opts(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8], state_ok: bool) -> Result<std::result::Result<Played, BlockVerdict>> {
    if let Some(pl) = played_cached(id) {
        return Ok(Ok((*pl).clone()));
    }
    let mut pl = Played::default();
    match check(id, &parent, seed, None, blob, Some(&mut pl), state_ok)? {
        BlockVerdict::Valid { .. } => {
            remember_played(*id, &pl);
            Ok(Ok(pl))
        }
        v => Ok(Err(v)),
    }
}
