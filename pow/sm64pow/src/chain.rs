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
}

/// How many end states to keep (about 8 MB each).
const CACHE: usize = 24;

struct Cache {
    map: HashMap<[u8; 32], Arc<EndState>>,
    order: VecDeque<[u8; 32]>,
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Cache { map: HashMap::new(), order: VecDeque::new() }))
}

pub fn cached(id: &[u8; 32]) -> Option<Arc<EndState>> {
    cache().lock().unwrap().map.get(id).cloned()
}

fn remember(id: [u8; 32], s: EndState) {
    let mut c = cache().lock().unwrap();
    if c.map.insert(id, Arc::new(s)).is_none() {
        c.order.push_back(id);
    }
    while c.order.len() > CACHE {
        if let Some(old) = c.order.pop_front() {
            c.map.remove(&old);
        }
    }
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

#[derive(Debug, Clone, PartialEq)]
pub enum BlockVerdict {
    Valid { frames: u32, progress: Progress, ends_cycle: bool },
    Invalid(String),
    /// The parent's end state isn't cached: replay its ancestors first.
    NeedParent,
}

/// Where a block with this proof start and parent begins: a game, the
/// progress it already has, or why the start isn't allowed (None: parent
/// state needed).
fn start_game(start: Start, parent: &Parent) -> Result<Option<std::result::Result<(sm64_binds::SM64Game, Progress), String>>> {
    let r = rules();
    let parent_state = match parent {
        Parent::Genesis => None,
        Parent::Block { id, .. } => cached(id),
    };
    match (start, parent) {
        (Start::PowerOn, Parent::Genesis) => {}
        (Start::Parent, Parent::Genesis) => return Ok(Some(Err("the first block starts from power-on".into()))),
        (Start::PowerOn, Parent::Block { gap_seconds, .. }) => {
            if *gap_seconds <= r.stall_seconds() {
                match &parent_state {
                    None => return Ok(None),
                    Some(s) if !s.ends_cycle => {
                        return Ok(Some(Err(format!(
                            "a power-on start needs a cycle-ending parent or {} s without a block",
                            r.stall_seconds()
                        ))));
                    }
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
                return Ok(Some(Ok((g, s.progress))));
            }
        },
    }
    let g = generator()?.create_game()?;
    let p = read_progress(&g)?;
    Ok(Some(Ok((g, p))))
}

/// Replays `pr` from `game` (progress `before`): Valid when the success
/// condition first holds on exactly the last frame with every input used.
/// `max_frames` None skips the time limit (rebuilding accepted blocks).
fn replay(game: sm64_binds::SM64Game, before: Progress, seed: &Seed, p: &Params, pr: &Proof, max_frames: Option<u32>, played: Option<&mut Vec<GamePad>>) -> Result<std::result::Result<(SM64MiningGame, Progress), String>> {
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
            o.push(pad);
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

fn check(id: &[u8; 32], parent: &Parent, seed: &Seed, difficulty: Option<u64>, blob: &[u8], played: Option<&mut Vec<GamePad>>) -> Result<BlockVerdict> {
    let p = params(difficulty.unwrap_or(1));
    let elapsed = match parent {
        Parent::Block { gap_seconds, .. } => *gap_seconds,
        Parent::Genesis => 0,
    };
    let limit = match difficulty {
        Some(d) => rules().max_frames(d, elapsed),
        None => proof::MAX_FRAMES as u32,
    };
    let pr = match proof::decode_proof(blob, limit as usize) {
        Ok(x) => x,
        Err(e) => return Ok(BlockVerdict::Invalid(format!("bad proof: {e}"))),
    };
    let (game, before) = match start_game(pr.start, parent)? {
        None => return Ok(BlockVerdict::NeedParent),
        Some(Err(why)) => return Ok(BlockVerdict::Invalid(why)),
        Some(Ok(x)) => x,
    };
    match replay(game, before, seed, &p, &pr, difficulty.map(|_| limit), played)? {
        Err(why) => Ok(BlockVerdict::Invalid(why)),
        Ok((g, now)) => {
            let ends_cycle = rules().ends_cycle(&now);
            remember(*id, EndState { game: g.game.save_state(), progress: now, ends_cycle });
            Ok(BlockVerdict::Valid { frames: pr.frames, progress: now, ends_cycle })
        }
    }
}

/// Verifies block `id` (its proof `blob`, seed and retarget difficulty: the
/// time limit also depends on the parent gap, see [`crate::Rules::max_frames`])
/// on top of `parent`, and caches its end state when valid.
pub fn verify_block(id: &[u8; 32], parent: Parent, seed: &Seed, difficulty: u64, blob: &[u8]) -> Result<BlockVerdict> {
    check(id, &parent, seed, Some(difficulty), blob, None)
}

/// Replays an already-accepted block to cache its end state (no time limit:
/// its difficulty was checked when it was accepted).
pub fn rebuild_block(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8]) -> Result<BlockVerdict> {
    check(id, &parent, seed, None, blob, None)
}

/// The pads a block's run played on every frame (pokes included), e.g. for
/// replays and `.m64` export. Needs the parent's end state, like verifying.
pub fn played_block(id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8]) -> Result<std::result::Result<Vec<GamePad>, BlockVerdict>> {
    let mut pads = Vec::new();
    match check(id, &parent, seed, None, blob, Some(&mut pads))? {
        BlockVerdict::Valid { .. } => Ok(Ok(pads)),
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
