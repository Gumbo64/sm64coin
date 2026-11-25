//! The end-state cache (layer 5): what the chain remembers between blocks.
//!
//! Verifying a block needs its parent's end state (a save state of a few
//! MB), so the last few dozen are kept in memory, keyed by run (parent,
//! start, seed, proof bytes) with block ids pointing at runs: blocks that
//! differ only in their timestamp are the same run and share one entry.
//! Run summaries (frames, progress, ...) of many more blocks are kept for the
//! parallel pre-pass. With [`set_state_dir`] the newest end states also go
//! to disk, so a restarted node verifies the next block without replaying
//! the cycle. A played-pads cache serves explorers (`get_sm64_cycle`).
//!
//! None of this decides validity: a missing entry only means a replay
//! (`BlockVerdict::NeedParent`, then the daemon rebuilds the parent).
//!
//! Read next: preverify.rs.

use crate::game::Progress;
use crate::proof::Start;
use crate::replay::{EndState, Parent, Played};
use crate::seed::Seed;
// Only the end states on disk (not in the browser build) need these.
#[cfg(not(target_arch = "wasm32"))]
use crate::{emulator::generator, game::read_progress};
#[cfg(not(target_arch = "wasm32"))]
use anyhow::Result;
#[cfg(not(target_arch = "wasm32"))]
use sm64_binds::SaveState;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

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
pub(crate) type RunKey = [u8; 32];

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
pub(crate) struct Summary {
    pub(crate) frames: u32,
    pub(crate) start: Start,
    pub(crate) progress: Progress,
    pub(crate) ends_cycle: bool,
    pub(crate) event: Option<crate::game::Event>,
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

/// The cached end state of `parent` (None for the genesis block, or when not cached).
pub(crate) fn parent_state(parent: &Parent) -> Option<Arc<EndState>> {
    match parent {
        Parent::Genesis => None,
        Parent::Block { id, .. } => cached(id),
    }
}

/// The cached end state of block `id` (the game's save state), gzip-compressed: for clients that
/// trust this node (the web miner starts its search from it instead of replaying the cycle).
/// None unless cached (never rebuilt here: answering a client must not cost a replay). The
/// last answer is kept (callers ask for the size first, then the bytes).
pub fn end_state_gz(id: &[u8; 32]) -> Option<Vec<u8>> {
    static LAST: Mutex<Option<([u8; 32], Vec<u8>)>> = Mutex::new(None);
    if let Some((k, v)) = LAST.lock().unwrap().as_ref() {
        if k == id {
            return Some(v.clone());
        }
    }
    let s = cached(id)?;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut enc, s.game.as_bytes()).ok()?;
    let gz = enc.finish().ok()?;
    *LAST.lock().unwrap() = Some((*id, gz.clone()));
    Some(gz)
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

pub(crate) fn known_summary(key: &RunKey) -> Option<Summary> {
    let c = cache().lock().unwrap();
    if let Some(r) = c.runs.get(key) {
        return Some(Summary { frames: r.frames, start: r.start, progress: r.state.progress, ends_cycle: r.state.ends_cycle, event: r.state.event });
    }
    c.sums.get(key).copied()
}

pub(crate) fn remember_summary(id: [u8; 32], key: RunKey, sum: Summary) {
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

pub(crate) fn run_key(parent: &Parent, start: Start, seed: &Seed, blob: &[u8]) -> RunKey {
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

pub(crate) fn alias(id: [u8; 32], key: RunKey) {
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

/// Caches a block's end state in memory; `durable` (a block just verified: the chain's newest)
/// also keeps it on disk, in the background. Replays of old blocks (rebuilds, a website asking
/// for played pads) stay in memory: writing them took ~0.3 s each (xz) and pushed the newest
/// blocks' states off the disk (the newest DISK_STATES files by time are kept).
pub(crate) fn remember(id: [u8; 32], key: RunKey, s: EndState, frames: u32, start: Start, durable: bool) {
    #[cfg(not(target_arch = "wasm32"))]
    if durable && STATE_DIR.get().is_some() {
        let copy = EndState { game: s.game.clone(), progress: s.progress, ends_cycle: s.ends_cycle, event: s.event };
        let _ = std::thread::Builder::new().name("sm64-persist".into()).spawn(move || persist(&id, &key, &copy, frames, start));
    }
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
pub(crate) static REPLAYS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

pub(crate) fn played_cached(id: &[u8; 32]) -> Option<Arc<Played>> {
    played_cache().lock().unwrap().runs.get(id).cloned()
}

pub(crate) fn remember_played(id: [u8; 32], pl: &Played) {
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

// ---- End states on disk: the last few blocks' states survive a restart -----

/// How many end states to keep on disk (the newest).
const DISK_STATES: usize = 8;

static STATE_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Keeps end states in `dir` from now on (the newest `DISK_STATES`), and loads
/// the ones already there, newest first. Returns how many were loaded. A node
/// that restarts then needs no replay of the cycle to verify the next block.
pub fn set_state_dir(dir: &std::path::Path) -> usize {
    // The browser build keeps no end states on disk.
    #[cfg(target_arch = "wasm32")]
    {
        let _ = dir;
        return 0;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
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
#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
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
