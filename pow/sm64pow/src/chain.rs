//! Blocks as a chain of game states (layer 5): the verdict per block.
//!
//! Each block's run starts where its parent's ended (or from power-on, see
//! [`proof::Start`]), so verifying a block needs its parent's end state.
//! [`verify_block`] looks the parent up in the cache ([`crate::cache`]); when
//! it isn't there it answers [`BlockVerdict::NeedParent`], and the daemon
//! (`Blockchain::sm64_rebuild`) walks back to a block it can start from and
//! replays forward with [`rebuild_block`] (difficulty 0: no time limit).
//! A run already verified (a restamped copy, or the pre-pass) is not
//! replayed again: only the timestamp-dependent rules are re-checked.
//!
//! The limit a block must meet is `rules().max_frames(difficulty, gap)`,
//! the gap being the seconds between its timestamp and its parent's (for a
//! restart, the gap minus the restart delay); the genesis block's child gets
//! the cap. This module re-exports the pieces below it so `chain::X` stays
//! the one path the daemon-facing code uses.
//!
//! Read next: ffi.rs (how the daemon calls this).

use crate::cache::{REPLAYS, alias, known_invalid, known_summary, parent_state, played_cached, remember, remember_invalid, remember_played, run_key};
use crate::game::Progress;
use crate::proof::{self, Start};
use crate::replay::{replay, replay_state, start_game};
use crate::rules::{active as rules, params};
use crate::seed::Seed;
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
                crate::admit::note_valid(seed, sum.frames);
                return Ok(BlockVerdict::Valid { frames: sum.frames, progress: sum.progress, ends_cycle: sum.ends_cycle });
            }
        }
    }
    let key = run_key(parent, start, seed, blob);
    // The time limit before the replay (so a replay's verdict never depends on
    // the timestamp, and can be remembered by run). Checked BEFORE the parent's
    // state is needed: a run over the limit, or one that already failed, must
    // never make the daemon rebuild a parent state (a replay of the parent's
    // cycle under the blockchain lock; DoS audit N1). A restart's limit (below)
    // is never larger, so the verdicts are unchanged.
    let claimed = match (&state, &pr) {
        (Some((frames, ..)), _) => *frames,
        (None, Some(pr)) => pr.frames,
        _ => unreachable!(),
    };
    if difficulty.is_some() && claimed > limit {
        return Ok(BlockVerdict::Invalid(format!("{claimed} frames exceeds the limit of {limit}")));
    }
    // The same run already failed a replay (the same block sent again, or a re-stamped copy).
    if let Some(why) = known_invalid(&key) {
        crate::admit::note_invalid();
        return Ok(BlockVerdict::Invalid(why));
    }
    let (game, before) = match start_game(start, parent, parent_state(parent), difficulty.is_some())? {
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
    // (the claimed length and a known-invalid run were checked before the parent's state was needed)
    if difficulty.is_some() && claimed > limit {
        return Ok(BlockVerdict::Invalid(format!("{claimed} frames exceeds the limit of {limit}")));
    }
    // RPC calls that replay draw on a budget (crate::admit::RpcScope).
    let timer = crate::admit::rpc_replay_gate().map_err(|e| anyhow::anyhow!(e))?;
    REPLAYS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (frames, result) = match (&state, &pr) {
        (Some((frames, _, c, body)), _) => (*frames, replay_state(game, before, seed, &p, *frames, *c, body, difficulty.map(|_| limit), played)),
        (None, Some(pr)) => (pr.frames, replay(game, before, seed, &p, pr, difficulty.map(|_| limit), played)),
        _ => unreachable!(),
    };
    crate::admit::rpc_replay_done(timer);
    match result? {
        Err(why) => {
            remember_invalid(key, &why);
            crate::admit::note_invalid();
            Ok(BlockVerdict::Invalid(why))
        }
        Ok((g, now)) => {
            let ends_cycle = rules().ends_cycle(&now);
            let event = crate::game::event(&before, &now);
            remember(*id, key, EndState { game: g.game.save_state(), progress: now, ends_cycle, event }, frames, start, difficulty.is_some());
            crate::admit::note_valid(seed, frames);
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

/// A block's signed run (sign.rs): the miner key its coinbase extra nonce
/// commits and what its signature signs. Err: the block is invalid (no key,
/// a bad signature, a proof whose header doesn't parse). No replay.
pub fn signed_run(seed: &Seed, blob: &[u8], extra_nonce: &[u8], sig: &[u8; 64]) -> std::result::Result<([u8; 32], crate::equivocation::SignedRun), String> {
    let frames = proof::split_blob(blob).map_err(|e| format!("bad proof: {e}"))?.0;
    let proof_id = crate::proof_id::proof_id(blob);
    let key = crate::sign::check(&seed.0, extra_nonce, &proof_id, frames, sig)?;
    crate::equivocation::policy().lock().unwrap().note_key(&seed.0, &key);
    Ok((key, crate::equivocation::SignedRun { proof_id, frames, sig: *sig }))
}

/// [`signed_run`] and the one-run-per-key rule (equivocation.rs): what this
/// signed run is for its template, the first run seen, a copy, or a second
/// different run (evidence is kept and queued for relay). No replay; the
/// block is not judged valid or invalid by it (consensus never rejects a
/// block for equivocation: only relay and building policy follow).
pub fn claim_signed_run(seed: &Seed, blob: &[u8], extra_nonce: &[u8], sig: &[u8; 64]) -> std::result::Result<crate::equivocation::Claim, String> {
    let (key, run) = signed_run(seed, blob, extra_nonce, sig)?;
    Ok(crate::equivocation::policy().lock().unwrap_or_else(|e| e.into_inner()).claim(&seed.0, &key, &run))
}

/// [`verify_block`] for a signed block (every block but the genesis block
/// from v0.3): the signature is checked first (about 0.1 ms), so a block
/// without a valid signature by its coinbase's miner key, a third party's
/// variant of someone else's run included, never costs a replay. A block that
/// passes also counts as its template's run for the one-run-per-key rule.
pub fn verify_signed_block(id: &[u8; 32], parent: Parent, seed: &Seed, difficulty: u64, blob: &[u8], extra_nonce: &[u8], sig: &[u8; 64]) -> Result<BlockVerdict> {
    if let Err(why) = claim_signed_run(seed, blob, extra_nonce, sig) {
        crate::admit::note_invalid();
        return Ok(BlockVerdict::Invalid(why));
    }
    verify_block(id, parent, seed, difficulty, blob)
}
