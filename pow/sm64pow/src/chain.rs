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
use crate::replay::{Committed, replay, replay_committed, replay_state, start_game};
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
    check_with(id, parent, seed, difficulty, blob, played, state_ok, None, &mut None)
}

/// [`check`]; with `committed` (v0.4) `blob` is the v0.3 layout of a committed proof
/// (`commit::layout`) whose stateless encoding already matched the header (`commit::open`):
/// runs are cached by the committed input hash and the layout, and a state codec is replayed
/// with `replay_committed` (a bad encoding sets `bad` and returns Invalid, which the caller
/// discards).
#[allow(clippy::too_many_arguments)]
fn check_with(id: &[u8; 32], parent: &Parent, seed: &Seed, difficulty: Option<u64>, blob: &[u8], played: Option<&mut Played>, state_ok: bool, committed: Option<&crate::commit::Header>, bad: &mut Option<String>) -> Result<BlockVerdict> {
    // A stateless layout here is the canonical run-length one (hash-checked by commit::open), so
    // the key is the run's. A state-coded body is only checked by its replay: keyed by its bytes
    // too, so a known run's hit never vouches for an encoding nobody has decoded (a node must
    // store and relay only encodings it verified). Same key as `prepare`.
    let key_src: Vec<u8> = match committed {
        Some(h) => [b"sm64cmt1".as_slice(), &h.input_hash, blob].concat(),
        None => blob.to_vec(),
    };
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
        let key = run_key(parent, start, seed, &key_src);
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
    let key = run_key(parent, start, seed, &key_src);
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
        (Some((frames, _, c, body)), _) => match committed {
            None => (*frames, replay_state(game, before, seed, &p, *frames, *c, body, difficulty.map(|_| limit), played)),
            Some(h) => (
                *frames,
                replay_committed(game, before, seed, &p, h, *c, body, difficulty.map(|_| limit), played).map(|c| match c {
                    Committed::Ok(g, now) => Ok((g, now)),
                    Committed::Invalid(why) => Err(why),
                    Committed::BadEncoding(why) => {
                        *bad = Some(why.clone());
                        Err(why)
                    }
                }),
            ),
        },
        (None, Some(pr)) => (pr.frames, replay(game, before, seed, &p, pr, difficulty.map(|_| limit), played)),
        _ => unreachable!(),
    };
    crate::admit::rpc_replay_done(timer);
    match result? {
        Err(why) => {
            if bad.is_some() {
                return Ok(BlockVerdict::Invalid(why)); // a bad encoding: nothing is remembered about the run
            }
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

/// A committed proof's verdict (v0.4, commit.rs): the run's, or a bad encoding of it.
#[derive(Debug, Clone, PartialEq)]
pub enum CommittedVerdict {
    /// About the committed run (every encoding of it gets the same verdict).
    Run(BlockVerdict),
    /// This blob doesn't decode to the committed inputs: drop the blob (and its sender), never
    /// the block id.
    BadEncoding(String),
}

/// Verifies block `id` whose proof is a committed blob (v0.4, docs/V0.4-DESIGN.md section 1):
/// the header decides the length (the limit at its stamp) and the start; the encoding must decode
/// to the committed inputs (stateless codecs before any replay, state codecs during it, every
/// frame); then the run is judged as v0.3 judges a run. Valid runs are cached by the committed
/// input hash (every stateless encoding of a known run is a cache hit; a state-coded body is a
/// hit only for the bytes that were replayed).
pub fn verify_committed_block(id: &[u8; 32], parent: Parent, seed: &Seed, difficulty: u64, blob: &[u8]) -> Result<CommittedVerdict> {
    committed(id, &parent, seed, Some(difficulty), blob, None, crate::statecodec::state_codec_enabled())
}

/// [`verify_committed_block`] with no time limit (rebuilding accepted blocks).
pub fn rebuild_committed_block(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8]) -> Result<CommittedVerdict> {
    committed(id, &parent, seed, None, blob, None, crate::statecodec::state_codec_enabled())
}

/// The pads a committed block's run played on every frame, and which were poked.
pub fn played_committed_block(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8]) -> Result<std::result::Result<Played, CommittedVerdict>> {
    let mut pl = Played::default();
    match committed(id, &parent, seed, None, blob, Some(&mut pl), crate::statecodec::state_codec_enabled())? {
        CommittedVerdict::Run(BlockVerdict::Valid { .. }) => Ok(Ok(pl)),
        v => Ok(Err(v)),
    }
}

/// A committed blob made ready for the v0.3-layout machinery: its header, the layout the replay
/// reads (stateless: the canonical run-length layout of the hash-checked run; state codec: the
/// blob without the hash) and the cache key source ([`check_with`]).
pub(crate) struct Prepared {
    pub hdr: crate::commit::Header,
    pub layout: Vec<u8>,
    pub key_src: Vec<u8>,
}

/// [`prepare`] under the active features: with committed inputs off the blob is its own layout
/// and key (v0.3), and `hdr` is None.
pub(crate) fn prepare_any(blob: &[u8], ceiling: u32, state_ok: bool) -> std::result::Result<(Option<crate::commit::Header>, Vec<u8>, Vec<u8>), crate::commit::Fault> {
    if !crate::features::committed() {
        return Ok((None, blob.to_vec(), blob.to_vec()));
    }
    prepare(blob, ceiling, state_ok).map(|p| (Some(p.hdr), p.layout, p.key_src))
}

pub(crate) fn prepare(blob: &[u8], ceiling: u32, state_ok: bool) -> std::result::Result<Prepared, crate::commit::Fault> {
    let (hdr, layout) = match crate::commit::open(blob, ceiling, state_ok)? {
        crate::commit::Opened::Run(h, pr) => (h, proof::encode_proof_with(&proof::RunLength, &pr)),
        crate::commit::Opened::State(h, ..) => (h, crate::commit::layout(blob).map_err(|e| crate::commit::Fault::BadEncoding(format!("{e:#}")))?),
    };
    let key_src = [b"sm64cmt1".as_slice(), &hdr.input_hash, &layout].concat();
    Ok(Prepared { hdr, layout, key_src })
}

fn committed(id: &[u8; 32], parent: &Parent, seed: &Seed, difficulty: Option<u64>, blob: &[u8], played: Option<&mut Played>, state_ok: bool) -> Result<CommittedVerdict> {
    let ceiling = match difficulty {
        Some(_) => rules().max_frames_any(),
        None => proof::MAX_FRAMES as u32,
    };
    let p = match prepare(blob, ceiling, state_ok) {
        Err(crate::commit::Fault::Invalid(why)) => return Ok(CommittedVerdict::Run(BlockVerdict::Invalid(why))),
        Err(crate::commit::Fault::BadEncoding(why)) => return Ok(CommittedVerdict::BadEncoding(why)),
        Ok(p) => p,
    };
    let mut bad = None;
    let v = check_with(id, parent, seed, difficulty, &p.layout, played, state_ok, Some(&p.hdr), &mut bad)?;
    if let BlockVerdict::Valid { .. } = v {
        crate::pof::note_size(&p.hdr, &p.layout);
    }
    Ok(match bad {
        Some(why) => CommittedVerdict::BadEncoding(why),
        None => CommittedVerdict::Run(v),
    })
}

/// The start every proof of a height must have (records.rs rule 8): power-on after the genesis
/// block or a cycle-ending block, else the parent's end state. `parent` is block h's parent.
/// None: that block's end state is not known (rebuild it first).
pub fn natural_start(parent: &Parent) -> Option<Start> {
    match parent {
        Parent::Genesis => Some(Start::PowerOn),
        Parent::Block { id, .. } => info(id).map(|(_, ends_cycle, _)| if ends_cycle { Start::PowerOn } else { Start::Parent }),
    }
}

/// What replaying a block's records found (docs/V0.4-DESIGN.md section 2.4 rule 9).
#[derive(Debug, Clone, PartialEq)]
pub enum RecordsVerdict {
    Valid,
    /// Record `i`'s run is invalid: the block is invalid.
    Invalid(usize, String),
    /// Record `i`'s encoding doesn't decode to its committed inputs: a bad witness.
    BadWitness(usize, String),
    /// The parent's end state isn't cached.
    NeedParent,
}

/// Replays the runs of records that passed `records::check` (their `facts`, their committed
/// proof blobs), all from `parent` (block h's parent, the parent of every proof), with no time
/// limit (a proof is bounded by the window instead), on up to `threads` threads. The verdict is
/// the first fault in record order; valid runs are cached like blocks (by input hash), so a
/// record seen earlier through proof gossip costs no replay.
pub fn verify_records(parent: Parent, facts: &[crate::records::Facts], proofs: &[&[u8]], threads: usize) -> Result<RecordsVerdict> {
    anyhow::ensure!(facts.len() == proofs.len(), "facts and proofs differ in length");
    let n = facts.len();
    let mut verdicts: Vec<Option<Result<CommittedVerdict>>> = (0..n).map(|_| None).collect();
    let threads = threads.clamp(1, n.max(1));
    std::thread::scope(|sc| {
        let chunks: Vec<_> = verdicts.chunks_mut(n.div_ceil(threads).max(1)).enumerate().collect();
        let per = n.div_ceil(threads).max(1);
        for (c, slot) in chunks {
            sc.spawn(move || {
                for (j, v) in slot.iter_mut().enumerate() {
                    let i = c * per + j;
                    let f = &facts[i];
                    let id = f.leaf(&[0; 64]); // a cache id per run (not a block id)
                    *v = Some(rebuild_committed_block(&id, parent, &Seed(f.seed), proofs[i]));
                }
            });
        }
    });
    for (i, v) in verdicts.into_iter().enumerate() {
        match v.unwrap()? {
            CommittedVerdict::Run(BlockVerdict::Valid { .. }) => {}
            CommittedVerdict::Run(BlockVerdict::Invalid(why)) => return Ok(RecordsVerdict::Invalid(i, why)),
            CommittedVerdict::Run(BlockVerdict::NeedParent) => return Ok(RecordsVerdict::NeedParent),
            CommittedVerdict::BadEncoding(why) => return Ok(RecordsVerdict::BadWitness(i, why)),
        }
    }
    Ok(RecordsVerdict::Valid)
}

/// A block's signed run (sign.rs): the miner key its coinbase extra nonce
/// commits and what its signature signs. Err: the block is invalid (no key,
/// a bad signature, a proof whose header doesn't parse). No replay.
pub fn signed_run(seed: &Seed, blob: &[u8], extra_nonce: &[u8], sig: &[u8; 64]) -> std::result::Result<([u8; 32], crate::equivocation::SignedRun), String> {
    // v0.4: the signature signs the committed header (commit.rs): no decoding (features.rs).
    let frames = crate::proof_id::claimed_frames(blob).map_err(|e| format!("bad proof: {e}"))?;
    let proof_id = crate::proof_id::active(blob);
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

/// [`verify_committed_block`] for a signed block (every block but the genesis block): the
/// signature over the committed header is checked first (about 0.1 ms, no decoding), so a block
/// without a valid signature by its coinbase's miner key never costs a replay (a third party
/// can't change the committed inputs without a new signature; it can only re-encode, which is
/// transport). A block that passes also counts as its template's run for the one-run-per-key rule.
pub fn verify_signed_block(id: &[u8; 32], parent: Parent, seed: &Seed, difficulty: u64, blob: &[u8], extra_nonce: &[u8], sig: &[u8; 64]) -> Result<CommittedVerdict> {
    if let Err(why) = claim_signed_run(seed, blob, extra_nonce, sig) {
        crate::admit::note_invalid();
        return Ok(CommittedVerdict::Run(BlockVerdict::Invalid(why)));
    }
    verify_any(id, parent, seed, difficulty, blob)
}

/// [`verify_committed_block`] with committed inputs on (features.rs), else v0.3's [`verify_block`].
pub fn verify_any(id: &[u8; 32], parent: Parent, seed: &Seed, difficulty: u64, blob: &[u8]) -> Result<CommittedVerdict> {
    if crate::features::committed() { verify_committed_block(id, parent, seed, difficulty, blob) } else { Ok(CommittedVerdict::Run(verify_block(id, parent, seed, difficulty, blob)?)) }
}

/// [`rebuild_committed_block`] or v0.3's [`rebuild_block`] (features.rs).
pub fn rebuild_any(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8]) -> Result<CommittedVerdict> {
    if crate::features::committed() { rebuild_committed_block(id, parent, seed, blob) } else { Ok(CommittedVerdict::Run(rebuild_block(id, parent, seed, blob)?)) }
}

/// [`played_committed_block`] or v0.3's [`played_wanted_block`] (features.rs).
pub fn played_any(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8]) -> Result<std::result::Result<Played, CommittedVerdict>> {
    if crate::features::committed() { played_committed_block(id, parent, seed, blob) } else { Ok(played_wanted_block(id, parent, seed, blob)?.map_err(CommittedVerdict::Run)) }
}
