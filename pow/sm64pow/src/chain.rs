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

/// A verified run without its end state: what the parallel pre-pass
/// ([`preverify`]) keeps for every block it replays (thousands fit), so the
/// sequential check only re-checks the timestamp-dependent rules.
#[derive(Clone, Copy)]
struct Summary {
    frames: u32,
    start: Start,
    progress: Progress,
    ends_cycle: bool,
    event: Option<crate::game::Event>,
}

/// How many run summaries to keep.
const SUMS: usize = 16384;

struct Cache {
    runs: HashMap<RunKey, Run>,
    order: VecDeque<RunKey>,
    ids: HashMap<[u8; 32], RunKey>,
    id_order: VecDeque<[u8; 32]>,
    sums: HashMap<RunKey, Summary>,
    sum_order: VecDeque<RunKey>,
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Cache { runs: HashMap::new(), order: VecDeque::new(), ids: HashMap::new(), id_order: VecDeque::new(), sums: HashMap::new(), sum_order: VecDeque::new() }))
}

pub fn cached(id: &[u8; 32]) -> Option<Arc<EndState>> {
    let c = cache().lock().unwrap();
    c.ids.get(id).and_then(|k| c.runs.get(k)).map(|r| r.state.clone())
}

/// A block's end progress, whether it ends the cycle, and its event: from its
/// cached end state or its run summary.
pub fn info(id: &[u8; 32]) -> Option<(Progress, bool, Option<crate::game::Event>)> {
    let c = cache().lock().unwrap();
    let k = c.ids.get(id)?;
    if let Some(r) = c.runs.get(k) {
        return Some((r.state.progress, r.state.ends_cycle, r.state.event));
    }
    c.sums.get(k).map(|s| (s.progress, s.ends_cycle, s.event))
}

fn known_summary(key: &RunKey) -> Option<Summary> {
    let c = cache().lock().unwrap();
    if let Some(r) = c.runs.get(key) {
        return Some(Summary { frames: r.frames, start: r.start, progress: r.state.progress, ends_cycle: r.state.ends_cycle, event: r.state.event });
    }
    c.sums.get(key).copied()
}

fn remember_summary(id: [u8; 32], key: RunKey, sum: Summary) {
    {
        let mut c = cache().lock().unwrap();
        if c.sums.insert(key, sum).is_none() {
            c.sum_order.push_back(key);
        }
        while c.sum_order.len() > SUMS {
            if let Some(old) = c.sum_order.pop_front() {
                c.sums.remove(&old);
            }
        }
    }
    alias(id, key);
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
    persist(&id, &key, &s, frames, start);
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
            remember(*id, key, EndState { game: g.game.save_state(), progress: now, ends_cycle, event }, frames, start);
            Ok(BlockVerdict::Valid { frames, progress: now, ends_cycle })
        }
    }
}

/// One block of a batch for [`preverify`].
pub struct BatchBlock {
    pub id: [u8; 32],
    pub parent: Parent,
    pub seed: Seed,
    pub blob: Vec<u8>,
}

/// Replays a proof blob from `game` with no time limit (the rules that depend
/// on the timestamp are checked later, by [`verify_block`]).
fn replay_blob(game: sm64_binds::SM64Game, before: Progress, seed: &Seed, blob: &[u8], state_ok: bool) -> Result<std::result::Result<(SM64MiningGame, Progress, u32), String>> {
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

/// The parallel pre-pass of a batch of blocks in chain order (a sync batch):
/// it splits the batch into chains that can be replayed on their own (from a
/// power-on start, the genesis block, or a parent whose end state is cached)
/// and replays the chains on up to `threads` threads at once, keeping a
/// [`Summary`] of every run and the full end state of each chain's last block.
/// [`verify_block`] then finds each run and only re-checks the rules that
/// depend on the timestamp and the parent, so a batch spanning several cycles
/// verifies in parallel. Nothing here decides validity: an invalid run is just
/// not summarized (and the chain stops there), and the sequential check
/// replays and rejects it as before. Returns how many runs it replayed.
pub fn preverify(batch: &[BatchBlock], threads: usize) -> usize {
    let state_ok = crate::statecodec::state_codec_enabled();
    // Chains: each block continues the previous one when it starts from its end state.
    let mut chains: Vec<Vec<&BatchBlock>> = Vec::new();
    for b in batch {
        let start = match proof::proof_start(&b.blob) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let parent_id = match b.parent {
            Parent::Block { id, .. } => Some(id),
            Parent::Genesis => None,
        };
        let continues = start == Start::Parent && chains.last().and_then(|c| c.last()).is_some_and(|l| Some(l.id) == parent_id);
        if continues {
            chains.last_mut().unwrap().push(b);
        } else if start == Start::PowerOn || parent_id.is_none() || parent_id.is_some_and(|p| cached(&p).is_some()) {
            chains.push(vec![b]);
        } else if start == Start::Parent {
            // Not startable here (no cached parent): the sequential check rebuilds it.
            chains.push(Vec::new());
        }
    }
    chains.retain(|c| !c.is_empty());
    let replayed = std::sync::atomic::AtomicUsize::new(0);
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..threads.max(1).min(chains.len()) {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(chain) = chains.get(i) else { break };
                let _ = replay_chain(chain, state_ok, &replayed);
            });
        }
    });
    replayed.into_inner()
}

fn replay_chain(chain: &[&BatchBlock], state_ok: bool, replayed: &std::sync::atomic::AtomicUsize) -> Result<()> {
    let mut prev: Option<(SaveState, Progress, bool)> = None;
    for (k, b) in chain.iter().enumerate() {
        let start = proof::proof_start(&b.blob)?;
        let key = run_key(&b.parent, start, &b.seed, &b.blob);
        let last = k + 1 == chain.len();
        if let Some(sum) = known_summary(&key) {
            // Already known: continue from its state if this chain needs it.
            if last {
                alias(b.id, key);
                return Ok(());
            }
            match cached(&b.id).or_else(|| { alias(b.id, key); cached(&b.id) }) {
                Some(s) => {
                    prev = Some((s.game.clone(), s.progress, s.ends_cycle));
                    continue;
                }
                None => {
                    let _ = sum;
                    // Its state is gone: replay it below like any other.
                }
            }
        }
        let (game, before) = match (start, &prev) {
            (Start::PowerOn, _) => {
                let g = generator()?.create_game()?;
                let p = read_progress(&g)?;
                (g, p)
            }
            (Start::Parent, Some((st, p, ends))) => {
                if *ends {
                    return Ok(());
                }
                let mut g = generator()?.create_game()?;
                g.load_state(st)?;
                (g, *p)
            }
            (Start::Parent, None) => match start_game(start, &b.parent, false)? {
                Some(Ok((g, p, _))) => (g, p),
                _ => return Ok(()),
            },
        };
        replayed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        REPLAYS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (g, now, frames) = match replay_blob(game, before, &b.seed, &b.blob, state_ok)? {
            Ok(x) => x,
            Err(_) => return Ok(()), // the sequential check rejects it
        };
        let ends_cycle = rules().ends_cycle(&now);
        let event = crate::game::event(&before, &now);
        let state = g.game.save_state();
        if last {
            remember(b.id, key, EndState { game: state, progress: now, ends_cycle, event }, frames, start);
        } else {
            remember_summary(b.id, key, Summary { frames, start, progress: now, ends_cycle, event });
            prev = Some((state, now, ends_cycle));
        }
    }
    Ok(())
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

/// Frames of played pads kept in memory (~5 bytes each). A block's played pads never change (its
/// id commits to its parent, seed and proof), and `get_sm64_cycle` asks for every block of the
/// cycle on each call: without this every call replayed the whole cycle (220 s on a Raspberry
/// Pi-class board at 23 blocks), and a website polling it every few seconds kept a node busy forever.
const PLAYED_FRAMES: usize = 8 << 20;

#[derive(Default)]
struct PlayedCache {
    runs: HashMap<[u8; 32], Arc<Played>>,
    order: VecDeque<[u8; 32]>,
    frames: usize,
}

fn played_cache() -> &'static Mutex<PlayedCache> {
    static C: OnceLock<Mutex<PlayedCache>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn played_cached(id: &[u8; 32]) -> Option<Arc<Played>> {
    played_cache().lock().unwrap().runs.get(id).cloned()
}

fn remember_played(id: [u8; 32], pl: &Played) {
    let mut c = played_cache().lock().unwrap();
    if c.runs.insert(id, Arc::new(pl.clone())).is_none() {
        c.order.push_back(id);
        c.frames += pl.pads.len();
    }
    while c.frames > PLAYED_FRAMES && c.order.len() > 1 {
        if let Some(old) = c.order.pop_front() {
            if let Some(r) = c.runs.remove(&old) {
                c.frames -= r.pads.len();
            }
        }
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
        // Past the start offset (crate::rules::STAR_CYCLE.start_halvings), where the limit grows every second.
        let o = crate::rules::STAR_CYCLE.start_halvings * crate::rules::STAR_CYCLE.decay_halving;
        // A run that fits the limit o + 30 s after the parent, not before.
        let frames = r.max_frames(d, o + 30);
        assert!(frames > r.max_frames(d, o + 29));
        // Stamped o + 30 s after the parent but arriving at o + 10: held until the node's clock says o + 30.
        assert_eq!(hold_until(d, parent, parent + o + 30, parent + o + 10, frames, 0), Some(parent + o + 30));
        // Stamped further ahead: still only valid from o + 30 on.
        assert_eq!(hold_until(d, parent, parent + o + 150, parent + o + 10, frames, 0), Some(parent + o + 30));
        // Arriving when it already fits: processed now.
        assert_eq!(hold_until(d, parent, parent + o + 30, parent + o + 31, frames, 0), None);
        // Too long even at its own timestamp: not held (the normal path rejects it).
        assert_eq!(hold_until(d, parent, parent + o + 20, parent + o + 10, frames, 0), None);
        // A restart: the limit is the limit `delay` seconds earlier.
        assert_eq!(hold_until(d, parent, parent + 3600 + o + 100, parent + 3600 + o + 10, frames, 3600), Some(parent + 3600 + o + 30));
        // At difficulty 1 the limit grows too (the start offset): a 2-minute run waits until it fits.
        let two_min = 2 * 60 * 30;
        let due = hold_until(1, parent, parent + o, parent, two_min, 0).expect("held at difficulty 1");
        assert!(r.max_frames(1, due - parent) >= two_min && r.max_frames(1, due - parent - 1) < two_min);
    }
}

// ---- End states on disk: the last few blocks' states survive a restart -----

/// How many end states to keep on disk (the newest).
const DISK_STATES: usize = 8;

static STATE_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Keeps end states in `dir` from now on (the newest [`DISK_STATES`]), and loads
/// the ones already there, newest first. Returns how many were loaded. A node
/// that restarts then needs no replay of the cycle to verify the next block.
pub fn set_state_dir(dir: &std::path::Path) -> usize {
    let _ = std::fs::create_dir_all(dir);
    let _ = STATE_DIR.set(dir.to_path_buf());
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "state"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort();
    let mut n = 0;
    // Oldest first, so the newest end up last in the cache's order.
    for (_, f) in files.iter().rev().take(DISK_STATES).rev() {
        if load_state_file(f).is_ok() {
            n += 1;
        }
    }
    n
}

fn event_bytes(e: Option<crate::game::Event>) -> [u8; 3] {
    match e {
        None => [0, 0, 0],
        Some(crate::game::Event::Star { course, star }) => [1, course, star],
        Some(crate::game::Event::Key(k)) => [2, k, 0],
        Some(crate::game::Event::GrandStar) => [3, 0, 0],
    }
}

fn event_from(b: [u8; 3]) -> Option<crate::game::Event> {
    match b[0] {
        1 => Some(crate::game::Event::Star { course: b[1], star: b[2] }),
        2 => Some(crate::game::Event::Key(b[1])),
        3 => Some(crate::game::Event::GrandStar),
        _ => None,
    }
}

const MAGIC: &[u8; 8] = b"SM64END1";

/// `<dir>/<id hex>.state`: magic, id, run key, frames, start, ends_cycle,
/// event, then the save state (xz).
fn persist(id: &[u8; 32], key: &RunKey, s: &EndState, frames: u32, start: Start) {
    let Some(dir) = STATE_DIR.get() else { return };
    let mut out = Vec::with_capacity(1 << 20);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(id);
    out.extend_from_slice(key);
    out.extend_from_slice(&frames.to_le_bytes());
    out.push(start as u8);
    out.push(s.ends_cycle as u8);
    out.extend_from_slice(&event_bytes(s.event));
    {
        use std::io::Write;
        let mut enc = xz2::write::XzEncoder::new(&mut out, 1);
        if enc.write_all(s.game.as_bytes()).is_err() || enc.finish().is_err() {
            return;
        }
    }
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    let path = dir.join(format!("{hex}.state"));
    let tmp = dir.join(format!("{hex}.tmp"));
    if std::fs::write(&tmp, &out).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
        // Keep the newest few.
        let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "state"))
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        files.sort();
        while files.len() > DISK_STATES {
            let _ = std::fs::remove_file(&files.remove(0).1);
        }
    }
}

fn load_state_file(path: &std::path::Path) -> Result<()> {
    use std::io::Read;
    let bytes = std::fs::read(path)?;
    anyhow::ensure!(bytes.len() > 8 + 32 + 32 + 4 + 2 + 3 && &bytes[..8] == MAGIC, "not an end-state file");
    let id: [u8; 32] = bytes[8..40].try_into()?;
    let key: RunKey = bytes[40..72].try_into()?;
    let frames = u32::from_le_bytes(bytes[72..76].try_into()?);
    let start = match bytes[76] {
        0 => Start::Parent,
        1 => Start::PowerOn,
        x => anyhow::bail!("bad start {x}"),
    };
    let ends_cycle = bytes[77] != 0;
    let event = event_from(bytes[78..81].try_into()?);
    let mut raw = Vec::new();
    xz2::read::XzDecoder::new(&bytes[81..]).read_to_end(&mut raw)?;
    let game = SaveState::from_bytes(raw)?;
    // The progress is read from the state itself.
    let mut g = generator()?.create_game()?;
    g.load_state(&game)?;
    let progress = read_progress(&g)?;
    {
        let mut c = cache().lock().unwrap();
        if c.runs.insert(key, Run { state: Arc::new(EndState { game, progress, ends_cycle, event }), frames, start }).is_none() {
            c.order.push_back(key);
        }
        while c.order.len() > CACHE {
            if let Some(old) = c.order.pop_front() {
                c.runs.remove(&old);
            }
        }
    }
    alias(id, key);
    Ok(())
}
