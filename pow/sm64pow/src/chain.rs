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

use crate::game::{Progress, SM64MiningGame, read_progress};
use crate::proof::{self, Proof, Start};
use crate::{Params, Seed, generator, params, rules};
use anyhow::Result;
use sm64_binds::{GamePad, SaveState};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

/// A block's end state: the game after its last frame, and its progress.
pub struct EndState {
    pub game: SaveState,
    pub progress: Progress,
    pub ends_cycle: bool,
    /// What the block's run added (bookkeeping: which star, key...).
    pub event: Option<crate::game::Event>,
}

/// How many end states to keep (about 8 MB each).
const CACHE: usize = 24;
/// How many block ids may point at cached runs. Blocks that differ only in
/// their timestamp (the seed excludes it, so anyone can re-stamp a published
/// block) are the same run: they share one end state and one cache slot, so a
/// flood of re-stamped variants can neither thrash the states of other blocks
/// nor make the node replay the same run again (red team finding F1).
const IDS: usize = 4096;

/// A run: what its result depends on (the parent's end state, the seed, and
/// the proof bytes). The block id additionally covers the timestamp.
type RunKey = [u8; 32];

/// A verified run, independent of the timestamp of the block carrying it.
struct Run {
    state: Arc<EndState>,
    frames: u32,
    start: Start,
}

struct Cache {
    runs: HashMap<RunKey, Run>,
    order: VecDeque<RunKey>,
    ids: HashMap<[u8; 32], RunKey>,
    id_order: VecDeque<[u8; 32]>,
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Cache { runs: HashMap::new(), order: VecDeque::new(), ids: HashMap::new(), id_order: VecDeque::new() }))
}

pub fn cached(id: &[u8; 32]) -> Option<Arc<EndState>> {
    let c = cache().lock().unwrap();
    c.ids.get(id).and_then(|k| c.runs.get(k)).map(|r| r.state.clone())
}

fn run_key(parent: &Parent, start: Start, seed: &Seed, blob: &[u8]) -> RunKey {
    use sha3::{Digest, Keccak256};
    let mut h = Keccak256::new();
    h.update(b"sm64run-key");
    match (start, parent) {
        // A power-on run does not depend on the parent (only on the seed).
        (Start::PowerOn, _) | (_, Parent::Genesis) => h.update([0u8; 32]),
        (Start::Parent, Parent::Block { id, .. }) => h.update(id),
    }
    h.update([start as u8]);
    h.update(seed.0);
    h.update(blob);
    h.finalize().into()
}

fn known_run(key: &RunKey) -> Option<(Arc<EndState>, u32, Start)> {
    cache().lock().unwrap().runs.get(key).map(|r| (r.state.clone(), r.frames, r.start))
}

fn alias(id: [u8; 32], key: RunKey) {
    let mut c = cache().lock().unwrap();
    if c.ids.insert(id, key).is_none() {
        c.id_order.push_back(id);
    }
    while c.id_order.len() > IDS {
        if let Some(old) = c.id_order.pop_front() {
            c.ids.remove(&old);
        }
    }
}

fn remember(id: [u8; 32], key: RunKey, s: EndState, frames: u32, start: Start) {
    {
        let mut c = cache().lock().unwrap();
        if c.runs.insert(key, Run { state: Arc::new(s), frames, start }).is_none() {
            c.order.push_back(key);
        }
        while c.order.len() > CACHE {
            if let Some(old) = c.order.pop_front() {
                c.runs.remove(&old);
            }
        }
    }
    alias(id, key);
}

/// How many runs this process has replayed (tests: a re-stamped variant is
/// not replayed again).
pub fn replays() -> u64 {
    REPLAYS.load(std::sync::atomic::Ordering::Relaxed)
}
static REPLAYS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

#[derive(Debug, Clone, PartialEq)]
pub enum BlockVerdict {
    Valid { frames: u32, progress: Progress, ends_cycle: bool },
    Invalid(String),
    /// The parent's end state isn't cached: replay its ancestors first.
    NeedParent,
}

/// Where a block with this proof start and parent begins: a game, the
/// progress it already has and how many seconds earlier a power-on restart's
/// limit is taken ([`crate::Rules::restart_delay`]), or why the start isn't
/// allowed (None: parent state needed). With `checking` false (rebuilding an
/// accepted block) a restart needs no parent state.
fn start_game(start: Start, parent: &Parent, checking: bool) -> Result<Option<std::result::Result<(sm64_binds::SM64Game, Progress, u64), String>>> {
    let r = rules();
    let parent_state = match parent {
        Parent::Genesis => None,
        Parent::Block { id, .. } => cached(id),
    };
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
fn replay(game: sm64_binds::SM64Game, before: Progress, seed: &Seed, p: &Params, pr: &Proof, max_frames: Option<u32>, played: Option<&mut Played>) -> Result<std::result::Result<(SM64MiningGame, Progress), String>> {
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
fn replay_state(game: sm64_binds::SM64Game, before: Progress, seed: &Seed, p: &Params, frames: u32, codec: &dyn crate::statecodec::StateCodec, body: &[u8], max_frames: Option<u32>, played: Option<&mut Played>) -> Result<std::result::Result<(SM64MiningGame, Progress), String>> {
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
            Ok(dec.pad(ft))
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

fn check(id: &[u8; 32], parent: &Parent, seed: &Seed, difficulty: Option<u64>, blob: &[u8], played: Option<&mut Played>, state_ok: bool) -> Result<BlockVerdict> {
    let p = params(difficulty.unwrap_or(1));
    let elapsed = match parent {
        Parent::Block { gap_seconds, .. } => *gap_seconds,
        Parent::Genesis => 0,
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
    // The same run already verified (e.g. a re-stamped copy of a block): only
    // the timestamp-dependent rules (start, above; time limit, here) can
    // differ, so don't replay it. Not for `played` (that wants the pads).
    let key = run_key(parent, start, seed, blob);
    if played.is_none() {
        if let Some((state, frames, s)) = known_run(&key) {
            if s == start {
                if difficulty.is_some() && frames > limit {
                    return Ok(BlockVerdict::Invalid(format!("{frames} frames exceeds the limit of {limit}")));
                }
                let (progress, ends_cycle) = (state.progress, state.ends_cycle);
                alias(*id, key);
                return Ok(BlockVerdict::Valid { frames, progress, ends_cycle });
            }
        }
    }
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
            remember(*id, key, EndState { game: g.game.save_state(), progress: now, ends_cycle, event }, frames, start);
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

pub fn played_wanted_block(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8]) -> Result<std::result::Result<Played, BlockVerdict>> {
    played_wanted_block_opts(id, parent, seed, blob, crate::statecodec::state_codec_enabled())
}

pub fn played_wanted_block_opts(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8], state_ok: bool) -> Result<std::result::Result<Played, BlockVerdict>> {
    let mut pl = Played::default();
    match check(id, &parent, seed, None, blob, Some(&mut pl), state_ok)? {
        BlockVerdict::Valid { .. } => Ok(Ok(pl)),
        v => Ok(Err(v)),
    }
}

/// For miners: a game at the start of a new block on top of `parent` with
/// proof start `start`, and the progress it starts with. `parent_game` is the
/// parent's end state (e.g. rebuilt by replaying the cycle's played pads from
/// power-on); ignored for a power-on start.
pub fn mining_start(start: Start, parent_game: Option<&SaveState>, seed: &Seed, p: &Params) -> Result<(SM64MiningGame, Progress)> {
    let mut g = generator()?.create_game()?;
    if start == Start::Parent {
        let s = parent_game.ok_or_else(|| anyhow::anyhow!("a parent start needs the parent's end state"))?;
        g.load_state(s)?;
    }
    let before = read_progress(&g)?;
    Ok((SM64MiningGame::from_game(g, seed, p), before))
}

/// The hold policy's decision (a node's relay policy, not consensus): when a
/// node may process a block extending its tip whose run is `frames` long, at
/// retarget `difficulty`, the parent timestamped `parent_ts`, the block
/// `block_ts`, the node's clock reading `now` (all unix seconds), and a
/// restart's `restart_delay` (the limit then is the limit that many seconds
/// earlier). `None`: process it now (it fits by the node's clock, or it is too
/// long even at its own timestamp and the normal path rejects it).
/// `Some(t)`: hold it until `t`, the first second the run fits by the node's
/// clock; stamping a block ahead of the clock therefore gains nothing.
pub fn hold_until(difficulty: u64, parent_ts: u64, block_ts: u64, now: u64, frames: u32, restart_delay: u64) -> Option<u64> {
    let r = rules();
    let limit_at = |t: u64| -> u32 {
        let gap = t.saturating_sub(parent_ts);
        if gap < restart_delay { 0 } else { r.max_frames(difficulty, gap - restart_delay) }
    };
    if frames <= limit_at(now) || frames > limit_at(block_ts) {
        return None;
    }
    let (mut lo, mut hi) = (now, block_ts.max(now + 1));
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if frames <= limit_at(mid) { hi = mid } else { lo = mid }
    }
    Some(hi)
}

#[cfg(test)]
mod hold_tests {
    use super::*;

    #[test]
    fn a_run_stamped_ahead_waits_for_the_nodes_clock() {
        let r = rules();
        let d = 1u64 << 12; // a difficulty whose limit is still growing
        let parent = 1_000_000u64;
        // A run that fits the limit 30 s after the parent, not before.
        let frames = r.max_frames(d, 30);
        assert!(frames > r.max_frames(d, 29));
        // Stamped 30 s after the parent but arriving 10 s after it: held until the node's clock says 30 s.
        assert_eq!(hold_until(d, parent, parent + 30, parent + 10, frames, 0), Some(parent + 30));
        // Stamped 120 s ahead (40 s after the parent's 30): still only valid from 30 s on.
        assert_eq!(hold_until(d, parent, parent + 150, parent + 10, frames, 0), Some(parent + 30));
        // Arriving when it already fits: processed now.
        assert_eq!(hold_until(d, parent, parent + 30, parent + 31, frames, 0), None);
        // Too long even at its own timestamp: not held (the normal path rejects it).
        assert_eq!(hold_until(d, parent, parent + 20, parent + 10, frames, 0), None);
        // A restart: the limit is the limit `delay` seconds earlier.
        assert_eq!(hold_until(d, parent, parent + 3700, parent + 3610, frames, 3600), Some(parent + 3630));
        // At difficulty 1 the limit starts at its maximum: nothing is ever held.
        assert_eq!(hold_until(1, parent, parent + 100, parent, r.max_frames(1, 0), 0), None);
    }
}
