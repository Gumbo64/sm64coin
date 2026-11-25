//! The RRT miner: the seg120 campaign (objective-rrt-v2's
//! tools/seg120/campaign.py), reimplemented for blocks. A block is one
//! progress point (a star, a key or the grand star), so a block is the
//! campaign's steps from the block's start to its next star.
//!
//! Every step is one zone-locked tree (objective-rrt-v2's engine, in a child
//! process: child.rs) with the campaign's settings ([`env`]):
//!
//! * in the castle: a castle star the save can have by now (`castle_star`),
//!   else the moat to drain (`moat_step`), else a tree that enters the course
//!   `want_target` names (a cap course whose switch is unpressed, else the
//!   open course with the most stars missing, least visited first), else
//!   one that enters any course not skipped (`no_go`). A target the castle
//!   didn't reach is skipped (and `open_fail`); when nothing is left the
//!   skip list is cleared and the budgets double (giving up above x8);
//! * in a course: a tree to the first state with more progress (in a cap
//!   course with its switch unpressed: with the switch pressed). No progress:
//!   a checkpoint (`cannon_cp`, `stage_cp`, `checkpoint`, chosen in the
//!   child) and a fresh tree from it, at most `checkpoints` in a row; then
//!   the course is skipped and the block goes back to the castle state it
//!   entered from (or out by the pause menu when the block started inside the
//!   course). A course given up after checkpoints is resumed from its last
//!   one when the castle enters it again.
//!
//! The campaign's state across steps (skip list, budget multiplier,
//! open_fail, castle/moat tries, dead-end checkpoints, visits) is kept across
//! blocks in `scratch/campaign.json`, per set of stars and keys had, with
//! campaign.py's rule on a new star: its course comes off the skip list, the
//! multiplier and open_fail reset. Not reimplemented: the 100-coin legs (a
//! 100-coin star comes only by chance), the castle hops (`OBRRT_ZONE_PATH`,
//! from a replay of the campaign's own route), the exits library, credits.
//!
//! This process owns the *shadow*: one `SM64MiningGame` that plays the block
//! for real, frame by frame, through `step` (the perturbed one, as the
//! verifier replays), records the played pads, ends the block on the first
//! frame the rules call success, and compares its position with the tree's at
//! every step boundary. The caller still verifies the sealed proof with
//! `sm64pow::chain`.
pub mod child;
pub mod plan;

use crate::rh::{Attempt, Mined, boot, settle};
use anyhow::{Result, bail};
use child::{CpAsk, Goal, Job, StepOut, is_hub_level};
use obrrt_core::{GamePad, St, read_state};
use plan::*;
use serde::{Deserialize, Serialize};
use sm64pow::proof::Start;
use sm64pow::sm64_binds::SaveState;
use sm64pow::{Params, SM64MiningGame, SM64MiningSave, Seed};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration}; use crate::sys::Instant;

#[derive(Clone)]
pub struct RrtOptions {
    pub threads: usize,
    /// Emulated frames (all threads) a castle tree / a course tree may use,
    /// before the multiplier (campaign.py --hub, --course).
    pub hub_frames: u64,
    pub course_frames: u64,
    /// Course trees stop after this many frames without new work in
    /// progress (--stall; 0: off).
    pub stall: u64,
    /// Checkpoints in a row before a course is given up (--checkpoints).
    pub checkpoints: usize,
    /// Wall-clock cap of one tree (0: none; the campaign has none).
    pub step_seconds: f64,
    pub mem_mb: usize,
    pub seed: u64,
    /// Frames a run may take (0: the most any difficulty allows).
    pub frame_cap: usize,
    /// Scratch directory for the step files, the campaign memory and the roadmap.
    pub scratch: PathBuf,
    /// The `rrtrun` binary (default: next to the running one).
    pub child: Option<PathBuf>,
    /// Keep the step logs.
    pub keep: bool,
    /// The ROM the child loads (default: sm64pow's search path).
    pub rom: Option<String>,
    /// More tree settings, "VAR=value VAR=value" (campaign.py --env).
    pub extra_env: String,
    /// Sharing with other searches of the same block (the web miner's workers): called as
    /// `exchange(kind, payload) -> Option<reply>`; see [`Exchange`]. None natively.
    pub exchange: Option<Exchange>,
    /// Tree sync between the workers (needs `exchange` to answer `"tree_sync"`): the period in seconds
    /// (0: off) and how many frontier states each offers per round (at most 16).
    pub sync_secs: f64,
    pub sync_k: usize,
    /// Sample the trees for a viewer (see `child::viz`; the web miner's page).
    pub viz: bool,
    /// Anytime mining: a run of this many frames is known (0: none), so only a shorter one counts (the frame cap
    /// is one less, and the trees go on past states that are too late, OBRRT_GOOD_MAX_FRAME) and a step that
    /// finds none ends the search ("no shorter run").
    pub beat: usize,
    /// The known run's objective keys as a goal bias (OBRRT_GUIDE_KEYS, weight `guide_weight`; empty: none).
    pub guide_keys: Vec<String>,
    pub guide_weight: f64,
    /// A run to go on from: its first frames (a prefix of a known run; replayed before the first step). The
    /// searches then look for a shorter end of it: one segment of the run attacked, the rest kept.
    pub prefix: Vec<GamePad>,
    /// A known run to branch off (see [`SeedRun`]), its nodes every `seed_chunk` frames added to the trees that
    /// start where the run's steps did, boosted by `seed_weight`.
    pub seed_run: Option<SeedRun>,
    pub seed_chunk: usize,
    pub seed_weight: f64,
}

/// A run found earlier (by this or another search of the block): its pads and the played length at the start of each step.
#[derive(Clone)]
pub struct SeedRun {
    pub pads: Vec<GamePad>,
    pub starts: Vec<usize>,
}

/// A channel to the other searches of the block. Kinds: `"step_get"` (payload: a step's key; reply:
/// the shared [`SharedStep`] JSON or None) and `"step_put"` (payload: [`SharedStep`] JSON, no
/// reply). The first castle step of the block is run once and shared: the other searches take its
/// outcome and go on with their own salts from there.
pub type Exchange = std::sync::Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>;

/// A campaign step's outcome shared between searches: `key` identifies the step's input (the job
/// without its seed, the start state's own path and out dir), so only an identical step is taken.
#[derive(Serialize, Deserialize)]
pub struct SharedStep {
    pub key: String,
    pub out: StepOut,
}

fn same_pads(a: &[GamePad], b: &[GamePad]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x.button, x.stick_x, x.stick_y) == (y.button, y.stick_x, y.stick_y))
}

fn cp_tag(played: &[GamePad], zone: u16, cp_n: usize) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    played.len().hash(&mut h);
    for p in played {
        (p.button, p.stick_x, p.stick_y).hash(&mut h);
    }
    format!("{:016x}-{zone}-{cp_n}", h.finish())
}

/// Switches the step's tree sync on (with `o.exchange`): the workers that are at the same point (same
/// played prefix, zone and checkpoint count) and run the same step input share one tag.
fn tree_sync(o: &RrtOptions, job: &mut Job, played: &[GamePad], zone: u16, cp_n: usize) {
    if o.exchange.is_some() && o.sync_secs > 0.0 && o.sync_k > 0 {
        job.sync_secs = o.sync_secs;
        job.sync_k = o.sync_k.min(16);
        job.sync_tag = format!("{}-{}", cp_tag(played, zone, cp_n), step_key(job));
    }
}

fn step_key(job: &Job) -> String {
    use std::hash::{Hash, Hasher};
    let mut j = job.clone();
    (j.seed, j.state_file, j.out_dir, j.viz) = (0, String::new(), String::new(), false);
    (j.sync_secs, j.sync_k, j.sync_tag) = (0.0, 0, String::new());
    // The anytime miner's settings change what a search looks for, not which step it is: a shared first castle step stays shared.
    j.env.retain(|(k, _)| !matches!(k.as_str(), "OBRRT_GOOD_MAX_FRAME" | "OBRRT_GUIDE_KEYS" | "OBRRT_GUIDE_WEIGHT"));
    (j.manual, j.manual_weight) = (Vec::new(), 0.0);
    j.max_path = 0;
    let text = serde_json::to_string(&j).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    format!("{:016x}", h.finish())
}

impl Default for RrtOptions {
    fn default() -> Self {
        RrtOptions {
            threads: 4,
            hub_frames: 60_000_000,
            course_frames: 150_000_000,
            stall: 0,
            checkpoints: 8,
            step_seconds: 0.0,
            mem_mb: 1500,
            seed: 1,
            frame_cap: 0,
            scratch: crate::sys::env::temp_dir().join("sm64coin-rrt"),
            child: None,
            keep: false,
            rom: None,
            extra_env: String::new(),
            exchange: None,
            sync_secs: 0.0,
            sync_k: 0,
            viz: false,
            beat: 0,
            guide_keys: Vec::new(),
            guide_weight: 20.0,
            prefix: Vec::new(),
            seed_run: None,
            seed_chunk: 45,
            seed_weight: 2.0,
        }
    }
}

pub fn coverage() -> Vec<(&'static str, &'static str)> {
    vec![
        ("star: any star the castle tree reaches and the course tree (with checkpoints) solves", "the seg120 campaign per block (castle -> course), most stars missing first"),
        ("star: castle stars (Toad, MIPS) once their gates are met", "castle-star step"),
        ("cap switches, the moat", "cap course first while its switch is unpressed; moat step after DDD 1"),
        ("key: Bowser 1 and 2", "any course the castle reaches, Bowser arenas as checkpoints"),
    ]
}

/// The campaign's tree settings: campaign.py's `ENV`, the t1 campaign's
/// `--env` (the priors and the roadmap from the miner's scratch), and `extra`.
pub fn env(o: &RrtOptions) -> Result<Vec<(String, String)>> {
    const ENV: &str = "OBRRT_SOFT_DEDUP=1 OBRRT_ROUND=64 OBRRT_BOOT_NOWAIT=1 OBRRT_TECH=all OBRRT_NEW_ELIGIBLE=1 OBRRT_STARSET_KEY=1 \
         OBRRT_PICK_BY_B=0 OBRRT_SWIM_HOLD=1 OBRRT_GP_CATCH=1 OBRRT_KEEP_WIP=1 OBRRT_MOVE_WEIGHT=0.75 OBRRT_CHAIN=route OBRRT_SAMPLER=rate \
         OBRRT_MOVES=1 OBRRT_MOVE_GA=1 OBRRT_MOVE_GA_EXTRA=1 OBRRT_CALM=1 OBRRT_LAKITU_ARC=1 OBRRT_PAINTING_FAST=1 OBRRT_ADD=last_gen \
         OBRRT_KINSHIP_MULT=10 OBRRT_ZONE_LOCK=1 \
         OBRRT_GOAL_FOCUS=0.5 OBRRT_GOAL_WIDE=1 OBRRT_GROUNDS_FIX=1 OBRRT_STATIC_ID=1 OBRRT_STALL_ETA=0.03 OBRRT_GOAL_EXPLORE=1";
    // Copies of objective-rrt-v2/tools/seg120/{reach,rescue}_prior.json (the t1 campaign's priors).
    const REACH: &str = include_str!("../../priors/reach_prior.json");
    const RESCUE: &str = include_str!("../../priors/rescue_prior.json");
    crate::sys::fs::create_dir_all(o.scratch.join("road"))?;
    let reach = o.scratch.join("reach_prior.json");
    let rescue = o.scratch.join("rescue_prior.json");
    crate::sys::fs::write(&reach, REACH)?;
    crate::sys::fs::write(&rescue, RESCUE)?;
    let mut v: Vec<(String, String)> = Vec::new();
    for kv in ENV.split_whitespace().chain(o.extra_env.split_whitespace()) {
        let (k, val) = kv.split_once('=').ok_or_else(|| anyhow::anyhow!("not VAR=value: {kv}"))?;
        v.push((k.into(), val.into()));
    }
    v.push(("OBRRT_MEM_MB".into(), o.mem_mb.to_string()));
    v.push(("OBRRT_ROAD".into(), o.scratch.join("road").display().to_string()));
    v.push(("OBRRT_REACH_PRIOR".into(), reach.display().to_string()));
    v.push(("OBRRT_RESCUE_PRIOR".into(), rescue.display().to_string()));
    v.push(("OBRRT_STOP_EXIT_GRACE".into(), "0".into()));
    Ok(v)
}

/// The campaign's state across steps, for one set of stars and keys had.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Memory {
    pub skip: Vec<u16>,
    pub mult: f64,
    pub open_fail: Vec<u16>,
    pub castle_try: Option<(i16, u8)>,
    pub moat_try: Option<(i16, u8)>,
    /// Per course level: its stars byte when they were taken, the (level,
    /// pos) of checkpoints that led nowhere, the stage kinds used.
    pub cp_seen: BTreeMap<u16, CpSeen>,
    /// Course levels entered from the castle, and how often.
    pub been: BTreeMap<u16, u32>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct CpSeen {
    pub stars: u8,
    pub at: Vec<(u16, [f32; 3])>,
    pub stages: Vec<String>,
}

/// `scratch/campaign.json`: [`Memory`] per signature (stars bytes and keys).
#[derive(Default, Serialize, Deserialize)]
struct MemoryFile {
    entries: BTreeMap<String, Memory>,
}

fn sig(st: &St) -> String {
    let mut s: String = st.course_stars.iter().map(|b| format!("{:02x}", b & 0x7f)).collect();
    s.push_str(&format!("k{}c{:x}", st.keys, st.save_flags & 0xe));
    s
}

fn sig_parts(s: &str) -> Option<([u8; 25], u8)> {
    let mut b = [0u8; 25];
    for (i, x) in b.iter_mut().enumerate() {
        *x = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    let k = s.get(51..)?.split('c').next()?.parse().ok()?;
    Some((b, k))
}

impl Memory {
    fn fresh() -> Memory {
        Memory { mult: 1.0, ..Default::default() }
    }

    /// The memory for the state `st`: its own, or the nearest earlier one's
    /// (a subset of its stars) with campaign.py's rule on what was gained:
    /// the courses that gained a star come off the skip list, the
    /// multiplier and open_fail reset.
    fn load(path: &PathBuf, st: &St) -> Memory {
        let f: MemoryFile = crate::sys::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let me = sig(st);
        if let Some(m) = f.entries.get(&me) {
            return m.clone();
        }
        let mut best: Option<(u32, &Memory, [u8; 25])> = None;
        for (k, m) in &f.entries {
            let Some((b, keys)) = sig_parts(k) else { continue };
            let subset = (0..25).all(|i| b[i] & !(st.course_stars[i] & 0x7f) == 0) && keys <= st.keys;
            let n: u32 = b.iter().map(|x| x.count_ones()).sum::<u32>() + keys as u32;
            if subset && best.as_ref().is_none_or(|x| n > x.0) {
                best = Some((n, m, b));
            }
        }
        let Some((_, m, b)) = best else { return Memory::fresh() };
        let mut m = m.clone();
        let gained: Vec<u16> = COURSE.iter().filter(|&&(_, c)| b[c as usize - 1] != st.course_stars[c as usize - 1] & 0x7f).map(|&(l, _)| l).collect();
        m.skip.retain(|l| !gained.contains(l));
        m.mult = 1.0;
        m.open_fail.clear();
        m
    }

    fn save(&self, path: &PathBuf, st: &St) {
        let mut f: MemoryFile = crate::sys::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        f.entries.insert(sig(st), self.clone());
        // The 64 with the most progress.
        while f.entries.len() > 64 {
            let k = f.entries.keys().min_by_key(|k| sig_parts(k).map_or(0, |(b, k)| b.iter().map(|x| x.count_ones()).sum::<u32>() + k as u32)).cloned().unwrap();
            f.entries.remove(&k);
        }
        if let Ok(t) = serde_json::to_string_pretty(&f) {
            let tmp = path.with_extension("json.tmp");
            if crate::sys::fs::write(&tmp, t).is_ok() {
                let _ = crate::sys::fs::rename(&tmp, path);
            }
        }
    }
}

/// A block's search in progress, kept so a later attempt at the same block
/// (same seed and start) goes on where this one stopped.
struct Resume {
    seed: [u8; 32],
    start: Start,
    game: SM64MiningSave,
    played: Vec<GamePad>,
    hub: Option<(SM64MiningSave, usize)>,
    cp_n: usize,
    cp_goal: Option<(String, String)>,
    /// Per course level: the last checkpoint's state, for a later visit.
    kept: BTreeMap<u16, (SM64MiningSave, Vec<GamePad>)>,
    sim: u64,
    salt: u64,
}

static RESUME: Mutex<Option<Resume>> = Mutex::new(None);

#[cfg(not(target_arch = "wasm32"))]
fn child_exe(o: &RrtOptions) -> Result<PathBuf> {
    if let Some(c) = &o.child {
        return Ok(c.clone());
    }
    let me = std::env::current_exe()?;
    let p = me.with_file_name(format!("rrtrun{}", std::env::consts::EXE_SUFFIX));
    if !p.exists() {
        bail!("{} not found (cargo build --release -p sm64coin-miner --bin rrtrun)", p.display());
    }
    Ok(p)
}

/// In the browser (the web miner) a step runs in a fresh WebAssembly instance of the same
/// module, which is what a fresh `rrtrun` process is natively (the search reads its settings
/// once per process): `globalThis.sm64miner.run_child(job_json)` (the worker's) runs
/// [`child::run_step`] there and returns its output as JSON. Files (the start state, the
/// roadmap, the priors) are on the shared JavaScript side (`crate::sys::fs`).
#[cfg(target_arch = "wasm32")]
fn run_child(o: &RrtOptions, game: &SM64MiningGame, mut job: Job, tag: &str, _stop: &AtomicBool) -> Result<StepOut> {
    #[wasm_bindgen::prelude::wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = sm64miner, js_name = run_child, catch)]
        fn js_run_child(job: &str) -> std::result::Result<String, wasm_bindgen::JsValue>;
    }
    let base = o.scratch.join(format!("step-{tag}"));
    let state = format!("{}.state", base.display());
    crate::sys::fs::write(&state, game.game.save_state().as_bytes())?;
    job.state_file = state.clone();
    job.pert = game.pert.clone();
    job.out_dir = format!("{}.run", base.display());
    let out = js_run_child(&serde_json::to_string(&job)?).map_err(|e| anyhow::anyhow!("step {tag}: {}", e.as_string().unwrap_or_else(|| format!("{e:?}"))));
    let _ = crate::sys::fs::remove_file(&state);
    let _ = crate::sys::fs::remove_dir_all(&job.out_dir);
    Ok(serde_json::from_str(&out?)?)
}

/// Frames simulated by this process's search steps so far (all threads, finished steps and the running ones' last
/// progress): the miner's speed is its rate (automine's `progress` line).
pub static SIM_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Runs the tree step `job` from `game` in a child process.
#[cfg(not(target_arch = "wasm32"))]
fn run_child(o: &RrtOptions, game: &SM64MiningGame, mut job: Job, tag: &str, stop: &AtomicBool) -> Result<StepOut> {
    crate::sys::fs::create_dir_all(&o.scratch)?;
    let base = o.scratch.join(format!("{}-{tag}", std::process::id()));
    let state = format!("{}.state", base.display());
    crate::sys::fs::write(&state, game.game.save_state().as_bytes())?;
    job.state_file = state.clone();
    job.pert = game.pert.clone();
    job.out_dir = format!("{}.run", base.display());
    let (jf, of, lf) = (format!("{}.job.json", base.display()), format!("{}.out.json", base.display()), format!("{}.log", base.display()));
    crate::sys::fs::write(&jf, serde_json::to_string(&job)?)?;
    let mut child = std::process::Command::new(child_exe(o)?)
        .arg(&jf)
        .arg(&of)
        .stdout(crate::sys::fs::File::create(&lf)?)
        .stderr(crate::sys::fs::File::options().append(true).open(&lf)?)
        .spawn()?;
    let pf = format!("{}.progress", job.out_dir);
    let mut counted = 0u64; // this step's frames already in SIM_FRAMES
    let mut polled = std::time::Instant::now();
    let status = loop {
        if polled.elapsed() >= Duration::from_millis(500) {
            polled = std::time::Instant::now();
            if let Ok(n) = crate::sys::fs::read_to_string(&pf).unwrap_or_default().trim().parse::<u64>() {
                if n > counted {
                    SIM_FRAMES.fetch_add(n - counted, Ordering::Relaxed);
                    counted = n;
                }
            }
        }
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if stop.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&pf);
            let _ = child.kill();
            let _ = child.wait();
            let _ = crate::sys::fs::remove_file(&state);
            bail!("stopped");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = crate::sys::fs::remove_file(&state);
    let _ = crate::sys::fs::remove_dir_all(&job.out_dir);
    let _ = std::fs::remove_file(&pf);
    if !status.success() {
        let tail: String = crate::sys::fs::read_to_string(&lf).unwrap_or_default().lines().rev().take(5).collect::<Vec<_>>().join(" | ");
        bail!("rrtrun {tag} failed ({status}): {tail}");
    }
    let out: StepOut = serde_json::from_str(&crate::sys::fs::read_to_string(&of)?)?;
    if out.emulated > counted {
        SIM_FRAMES.fetch_add(out.emulated - counted, Ordering::Relaxed);
    }
    let _ = crate::sys::fs::remove_file(&of);
    let _ = crate::sys::fs::remove_file(&jf);
    if !o.keep {
        let _ = crate::sys::fs::remove_file(&lf);
    }
    Ok(out)
}

/// Plays `pads` on the shadow, stopping at the first success.
/// Ok(true): the block's progress point was made (`played` ends there).
fn replay(shadow: &mut SM64MiningGame, played: &mut Vec<GamePad>, pads: &[GamePad], before: &sm64pow::game::Progress) -> Result<bool> {
    let rules = sm64pow::rules();
    for p in pads {
        played.push(shadow.step(*p)?.0);
        if rules.is_success(before, &shadow.progress()?) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Idles until Mario is controllable or a menu is open (the next tree's root).
fn idle_until_controllable(game: &mut SM64MiningGame, played: &mut Vec<GamePad>, max: usize, before: &sm64pow::game::Progress) -> Result<bool> {
    let rules = sm64pow::rules();
    let mut st = read_state(game, played.len() as u32, false);
    // A menu (the act select screen) is the tree's to handle (SelectAct objectives), not idled through.
    while !st.controllable() && !st.modal() {
        played.push(game.step(GamePad::default())?.0);
        if rules.is_success(before, &game.progress()?) {
            return Ok(true);
        }
        st = read_state(game, played.len() as u32, false);
        if played.len() >= max {
            bail!("Mario never became controllable after the tree's exit");
        }
    }
    Ok(false)
}

fn guard(shadow: &SM64MiningGame, played: &[GamePad], level: u16, frame: u32, pos: [f32; 3]) -> Result<()> {
    let ss = read_state(shadow, played.len() as u32, false);
    if ss.pos != pos || ss.level != level || ss.frame != frame {
        bail!(
            "the tree's state and the perturbed replay diverge at frame {} (pos {:?} level {} vs tree pos {:?} level {} frame {}): a game was stepped unperturbed",
            played.len(), ss.pos, ss.level, pos, level, frame
        );
    }
    Ok(())
}

/// campaign.py `pause_exit`: START, the stick down to "Exit Course", A, then
/// idle until Mario is in control in the castle. Ok(false): not there (a
/// poke may have moved the cursor), with the game and `played` as they were.
fn pause_exit(game: &mut SM64MiningGame, played: &mut Vec<GamePad>, max: usize, before: &sm64pow::game::Progress) -> Result<bool> {
    let save = (game.save_state(), played.len());
    let mut tail = vec![GamePad::new(0x1000, 0, 0)];
    tail.extend(std::iter::repeat_n(GamePad::default(), 30));
    tail.extend(std::iter::repeat_n(GamePad::new(0, 0, -80), 3));
    tail.extend(std::iter::repeat_n(GamePad::default(), 10));
    tail.push(GamePad::new(0x8000, 0, 0));
    let rules = sm64pow::rules();
    for (i, p) in tail.iter().chain(std::iter::repeat(&GamePad::default())).enumerate() {
        if i >= tail.len() + 400 || played.len() >= max {
            break;
        }
        played.push(game.step(*p)?.0);
        if rules.is_success(before, &game.progress()?) {
            bail!("progress during the pause-menu exit");
        }
        let st = read_state(game, played.len() as u32, false);
        if i >= tail.len() && is_hub_level(st.level) && st.controllable() && st.play_mode == 0 {
            return Ok(true);
        }
    }
    game.load_state(&save.0)?;
    played.truncate(save.1);
    Ok(false)
}

/// Mines one block from `start` to the next progress point.
pub fn mine(start: Start, parent: Option<&SaveState>, seed: &Seed, params: &Params, o: &RrtOptions, stop: &AtomicBool, mut log: impl FnMut(String)) -> Result<Attempt> {
    let t0 = Instant::now();
    let mut max = if o.frame_cap > 0 { o.frame_cap } else { sm64pow::rules().max_frames_any() as usize };
    if o.beat > 0 {
        max = max.min(o.beat - 1);
    }
    let (mut game, before) = sm64pow::chain::mining_start(start, parent, seed, params)?;
    let mut played: Vec<GamePad> = Vec::new();
    let st = match start {
        Start::PowerOn => boot(&mut game, &mut played, max)?,
        Start::Parent => settle(&mut game, &mut played, max)?,
    };
    let origin = st.clone();
    log(format!("start {:?} at {} stars {} keys, ready at frame {} (level {} {})", start, before.stars, before.keys, st.frame, st.level, name(st.level)));
    let mem_path = o.scratch.join("campaign.json");
    crate::sys::fs::create_dir_all(&o.scratch)?;
    let mut mem = Memory::load(&mem_path, &origin);
    let mut env0 = env(o)?;
    if o.beat > 0 {
        env0.push(("OBRRT_GOOD_MAX_FRAME".into(), (o.beat - 1).to_string()));
    }
    if !o.guide_keys.is_empty() {
        let gp = o.scratch.join("guide_keys.txt");
        crate::sys::fs::write(&gp, o.guide_keys.join("\n"))?;
        env0.push(("OBRRT_GUIDE_KEYS".into(), gp.display().to_string()));
        env0.push(("OBRRT_GUIDE_WEIGHT".into(), o.guide_weight.to_string()));
    }
    let rom = match &o.rom {
        Some(r) => crate::sys::fs::canonicalize(r)?.to_string_lossy().into_owned(),
        None => sm64pow::default_rom_path().ok_or_else(|| anyhow::anyhow!("no ROM (RrtOptions::rom)"))?,
    };
    // Where the block's search was, if an earlier attempt at this block stopped.
    let mut hub: Option<(SM64MiningSave, usize)> = None;
    let mut cp_n = 0usize;
    let mut cp_goal: Option<(String, String)> = None;
    let mut kept: BTreeMap<u16, (SM64MiningSave, Vec<GamePad>)> = BTreeMap::new();
    let mut sim = 0u64;
    let mut salt = o.seed;
    if let Some(r) = RESUME.lock().unwrap().take() {
        if r.seed == seed.0 && r.start == start {
            game.load_state(&r.game)?;
            played = r.played;
            hub = r.hub;
            cp_n = r.cp_n;
            cp_goal = r.cp_goal;
            kept = r.kept;
            sim = r.sim;
            salt = r.salt;
            log(format!("resuming this block's search at frame {} (level {})", played.len(), name(read_state(&game, played.len() as u32, false).level)));
        }
    }
    if o.prefix.len() > played.len() {
        anyhow::ensure!(same_pads(&o.prefix[..played.len()], &played), "the run to go on from does not start like this block's start");
        let rest = o.prefix[played.len()..].to_vec();
        if replay(&mut game, &mut played, &rest, &before)? {
            bail!("the run to go on from already makes the progress point at frame {}", played.len());
        }
        let s = read_state(&game, played.len() as u32, false);
        log(format!("going on from the best run's frame {} (level {} {})", played.len(), s.level, name(s.level)));
    }
    // The objectives along the stages' winning paths (the guide for the next search), and where the steps started.
    let mut keys: Vec<String> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    let finish = |played: Vec<GamePad>, sim: u64, keys: Vec<String>, starts: Vec<usize>| Attempt::Found(Mined { pads: played, seconds: t0.elapsed().as_secs_f64(), sim_frames: sim, keys, starts });
    let mut step = 0usize;
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            *RESUME.lock().unwrap() = Some(Resume { seed: seed.0, start, game: game.save_state(), played: played.clone(), hub: hub.clone(), cp_n, cp_goal: cp_goal.clone(), kept: kept.clone(), sim, salt });
            break Attempt::Gave("stopped".into());
        }
        mem.save(&mem_path, &origin);
        step += 1;
        salt += 1;
        starts.push(played.len());
        let st = read_state(&game, played.len() as u32, true);
        let in_hub = is_hub_level(st.level);
        let mut env = env0.clone();
        let mut job = Job {
            rom: rom.clone(),
            state_file: String::new(),
            pert: game.pert.clone(),
            frame0: played.len() as u32,
            goal: Goal::Enter,
            target: None,
            need_flags: 0,
            skip: Vec::new(),
            env: Vec::new(),
            budget: 0,
            seconds: o.step_seconds,
            threads: o.threads,
            seed: salt,
            max_path: (max - played.len()) as u32,
            out_dir: String::new(),
            checkpoint: None,
            viz: o.viz,
            sync_secs: 0.0,
            sync_k: 0,
            sync_tag: String::new(),
            manual: Vec::new(),
            manual_weight: 0.0,
        };
        // A known run that this step starts like: the tree gets its nodes (every `seed_chunk` frames, none at its last frame).
        if let Some(sr) = &o.seed_run {
            let s0 = played.len();
            if let Some(i) = sr.starts.iter().position(|&x| x == s0).filter(|_| sr.pads.len() >= s0 && same_pads(&sr.pads[..s0], &played)) {
                let end = sr.starts.get(i + 1).copied().unwrap_or(sr.pads.len()).min(sr.pads.len());
                let seg: Vec<(u16, i8, i8)> = sr.pads[s0..end].iter().map(|p| (p.button, p.stick_x, p.stick_y)).collect();
                let c = o.seed_chunk.max(1);
                job.manual = (1..).map(|k| k * c).take_while(|&n| n < seg.len()).map(|n| seg[..n].to_vec()).collect();
                job.manual_weight = o.seed_weight;
            }
        }
        if in_hub || GEO_LEVELS.contains(&st.level) {
            env.push(("OBRRT_GEO".into(), "1".into()));
        }
        if final_(&st) && (st.level == BITS || st.level == arena(BITS)) {
            env.push(("OBRRT_FINAL".into(), "1".into()));
        }
        if in_hub {
            let castle = castle_star(&st, &mem);
            let moat = if castle.is_none() && !final_(&st) { moat_step(&st, &mem) } else { false };
            let want = want_target(&st, &mem);
            job.budget = (o.hub_frames as f64 * mem.mult) as u64;
            let what;
            if let Some(area) = castle {
                job.goal = Goal::Progress;
                job.skip = all_courses();
                env.push(("OBRRT_STOP_PROGRESS".into(), "1".into()));
                what = format!("castle star in {}.{}", area.0, area.1);
            } else if moat {
                job.goal = Goal::Flag;
                job.need_flags = MOAT;
                job.skip = all_courses();
                job.budget = 3 * o.hub_frames;
                env.push(("OBRRT_STOP_FLAG".into(), format!("{MOAT:#x}")));
                what = "drain the moat (basement pillars)".into();
            } else if let Some(w) = want {
                let via = via(w);
                job.target = Some(via);
                job.skip = all_courses().into_iter().filter(|&l| l != via).collect();
                what = format!("to {} ({})", name(via), if CAP.iter().any(|c| c.0 == w) { "cap switch" } else { "most stars missing" });
            } else {
                job.skip = no_go(&st, &mem);
                what = format!("any course but {:?}", job.skip.iter().map(|&l| name(l)).collect::<Vec<_>>());
            }
            job.env = env;
            log(format!("step {step}: castle at frame {}, {} stars {} keys: {what}; budget {:.0} M frames", played.len(), st.stars, st.keys, job.budget as f64 / 1e6));
            let ckpt = (game.save_state(), played.len());
            tree_sync(o, &mut job, &played, st.level, cp_n);
            // The block's first castle step is the same for every search of it: shared.
            let key = (step == 1 && o.exchange.is_some()).then(|| step_key(&job));
            let shared: Option<StepOut> = key.as_ref().zip(o.exchange.as_ref()).and_then(|(k, ex)| {
                let r: SharedStep = serde_json::from_str(&ex("step_get", k)?).ok()?;
                (r.key == *k).then_some(r.out)
            });
            let out = match shared {
                Some(x) => {
                    log("  taking the castle step another search made (shared)".into());
                    x
                }
                None => {
                    let x = match run_child(o, &game, job.clone(), &format!("s{step}"), stop) {
                        Ok(x) => x,
                        Err(e) if stop.load(Ordering::Relaxed) => {
                            *RESUME.lock().unwrap() = Some(Resume { seed: seed.0, start, game: game.save_state(), played: played.clone(), hub: hub.clone(), cp_n, cp_goal: cp_goal.clone(), kept: kept.clone(), sim, salt });
                            break Attempt::Gave(format!("stopped ({e})"));
                        }
                        Err(e) => return Err(e),
                    };
                    if let (Some(k), Some(ex)) = (&key, &o.exchange) {
                        if let Ok(t) = serde_json::to_string(&SharedStep { key: k.clone(), out: x.clone() }) {
                            ex("step_put", &t);
                        }
                    }
                    x
                }
            };
            sim += out.emulated;
            if let Some(y) = &out.sync {
                log(format!("  tree sync: {} rounds, offered {}, received {}, added {} (dup {}, known {}, bad {}, errors {}), {} frames replayed; imported nodes expanded {}, on the winner's path {}", y.rounds, y.offered, y.received, y.added, y.dup, y.known, y.bad, y.errors, y.frames, y.expanded, y.on_path));
            }
            if !out.found {
                log(format!("  {} ({:.0} s)", out.note, t0.elapsed().as_secs_f64()));
                if o.beat > 0 {
                    break Attempt::Gave("no shorter run".into());
                }
                if castle.is_some() {
                    mem.castle_try = Some((st.stars, st.keys));
                } else if moat {
                    mem.moat_try = Some((st.stars, st.keys));
                } else if let Some(w) = want {
                    // The cap course (or the open course) wasn't reached: given up for now.
                    if !mem.skip.contains(&w) {
                        mem.skip.push(w);
                    }
                    if !CAP.iter().any(|c| c.0 == w) {
                        mem.open_fail.push(w);
                    }
                    log(format!("  castle did not reach {}; skipping it", name(w)));
                } else {
                    // Nothing left that the castle reaches and we haven't given up on: try harder.
                    mem.skip.clear();
                    mem.mult *= 2.0;
                    log(format!("  castle found no new course; budgets x{}", mem.mult));
                    if mem.mult > 8.0 {
                        mem.save(&mem_path, &origin);
                        break Attempt::Gave("budgets above x8".into());
                    }
                }
                continue;
            }
            log(format!("  {} at frame {} (+{}), {:.1} M frames, {} nodes ({:.0} s)", name(out.level), out.frame, out.pads.len(), out.emulated as f64 / 1e6, out.nodes, t0.elapsed().as_secs_f64()));
            keys.extend(out.path_keys.iter().cloned());
            if replay(&mut game, &mut played, &child::pads_of(&out.pads), &before)? {
                break finish(played, sim, keys, starts);
            }
            guard(&game, &played, out.level, out.frame, out.pos)?;
            if job.goal == Goal::Flag {
                log("  MOAT DRAINED".into());
                continue;
            }
            if is_hub_level(out.level) {
                bail!("the castle tree's winner is in the castle without progress");
            }
            if idle_until_controllable(&mut game, &mut played, max, &before)? {
                break finish(played, sim, keys, starts);
            }
            *mem.been.entry(out.level).or_default() += 1;
            hub = Some(ckpt);
            cp_n = 0;
            cp_goal = None;
            // A course given up after checkpoints is resumed from its last one.
            if let Some((g, p)) = kept.get(&out.level) {
                let s = read_state(&g.game_view()?.game, p.len() as u32, false);
                if sig(&s) == sig(&read_state(&game, played.len() as u32, false)) {
                    game.load_state(g)?;
                    played = p.clone();
                    log(format!("  resuming {} from its last checkpoint (frame {})", name(out.level), played.len()));
                }
            }
            continue;
        }
        // In a course.
        let zone = st.level;
        let need = CAP.iter().find(|c| c.0 == zone).map_or(0, |c| c.1) & !st.save_flags;
        job.goal = Goal::Progress;
        job.need_flags = need;
        job.budget = (o.course_frames as f64 * mem.mult) as u64;
        if need == 0 {
            env.push(("OBRRT_STOP_PROGRESS".into(), "1".into()));
        }
        if o.stall > 0 {
            env.push(("OBRRT_STOP_STALL".into(), o.stall.to_string()));
        }
        job.skip = no_go(&st, &mem);
        let seen = mem.cp_seen.get(&zone).filter(|s| s.stars == course_byte(&st, zone)).cloned().unwrap_or_default();
        job.checkpoint = (cp_n < o.checkpoints).then(|| CpAsk { n: cp_n, prefer: if cp_n > 0 { cp_goal.clone() } else { None }, avoid: seen.at.clone(), used_stages: seen.stages.clone(), keys: st.keys, end: final_(&st) });
        job.env = env;
        log(format!("step {step}: {} at frame {} (act select {} controllable {} action {:#x}){}{}; budget {:.0} M frames", name(zone), played.len(), st.act_select, st.controllable(), st.action, if need != 0 { format!(", cap switch {need:#x} wanted") } else { String::new() }, if cp_n > 0 { format!(", after checkpoint {cp_n}") } else { String::new() }, job.budget as f64 / 1e6));
        tree_sync(o, &mut job, &played, zone, cp_n);
        let out = match run_child(o, &game, job, &format!("s{step}"), stop) {
            Ok(x) => x,
            Err(e) if stop.load(Ordering::Relaxed) => {
                *RESUME.lock().unwrap() = Some(Resume { seed: seed.0, start, game: game.save_state(), played: played.clone(), hub: hub.clone(), cp_n, cp_goal: cp_goal.clone(), kept: kept.clone(), sim, salt });
                break Attempt::Gave(format!("stopped ({e})"));
            }
            Err(e) => return Err(e),
        };
        sim += out.emulated;
        if let Some(y) = &out.sync {
            log(format!("  tree sync: {} rounds, offered {}, received {}, added {} (dup {}, known {}, bad {}, errors {}), {} frames replayed; imported nodes expanded {}, on the winner's path {}", y.rounds, y.offered, y.received, y.added, y.dup, y.known, y.bad, y.errors, y.frames, y.expanded, y.on_path));
        }
        if out.found {
            log(format!("  progress at frame {} (+{}), {:.1} M frames, {} nodes ({:.0} s)", out.frame, out.pads.len(), out.emulated as f64 / 1e6, out.nodes, t0.elapsed().as_secs_f64()));
            keys.extend(out.path_keys.iter().cloned());
            if replay(&mut game, &mut played, &child::pads_of(&out.pads), &before)? {
                break finish(played, sim, keys, starts);
            }
            bail!("the course tree's winner made progress but the perturbed replay did not (frame {}): a game was stepped unperturbed", played.len());
        }
        let mut ck = out.checkpoint;
        // The searches of this block that reached the same point with the same played pads
        // (the block's shared prefix) offer their checkpoints through the exchange: the one
        // nearest its goal (lowest ETA) is taken by all, so the searches stay together on the
        // most promising line (the tree itself isn't shared). The peer's pads replay on the same
        // state; should they not reach the same place, this search's own checkpoint stays.
        if let (Some(c), Some(ex)) = (ck.as_ref(), o.exchange.as_ref()) {
            let tag = cp_tag(&played, zone, cp_n);
            let msg = serde_json::json!({ "tag": tag, "eta": c.eta, "cp": c });
            if let Some(best) = ex("cp_sync", &msg.to_string()).and_then(|r| serde_json::from_str::<child::Cp>(&r).ok()) {
                if best.pads != c.pads {
                    let snap = (game.save_state(), played.len());
                    let before_n = played.len();
                    let ok = !best.pads.is_empty() && replay(&mut game, &mut played, &child::pads_of(&best.pads), &before).ok() == Some(false) && guard(&game, &played, best.level, best.frame, best.pos).is_ok();
                    game.load_state(&snap.0)?;
                    played.truncate(before_n);
                    if ok {
                        log(format!("  taking the checkpoint another search found (ETA {:.0} < {:.0})", best.eta, c.eta));
                        ck = Some(best);
                    }
                }
            }
        }
        if let Some(cp) = ck {
            // No progress, but further along a goal: a fresh tree goes on from there.
            if replay(&mut game, &mut played, &child::pads_of(&cp.pads), &before)? {
                break finish(played, sim, keys, starts);
            }
            guard(&game, &played, cp.level, cp.frame, cp.pos)?;
            cp_n += 1;
            let e = mem.cp_seen.entry(zone).or_default();
            if e.stars != course_byte(&st, zone) {
                *e = CpSeen { stars: course_byte(&st, zone), ..Default::default() };
            }
            e.at.push((cp.level, cp.pos));
            if cp.how == "stage" {
                e.stages.push(cp.kind.clone());
            }
            cp_goal = cp.key.clone().filter(|_| cp.how != "moved").map(|k| (k, cp.how.clone()));
            kept.insert(zone, (game.save_state(), played.clone()));
            log(format!(
                "  {}; checkpoint {cp_n} at node {}, frame {}, {} {:?}: {} ({}) ETA {} -> {:.0}, wip {} -> {}",
                out.note, cp.node, cp.frame, name(cp.level), cp.pos.map(|x| x.round()), cp.kind, cp.how, cp.eta0.map_or("none".into(), |e| format!("{e:.0}")), cp.eta, cp.wip0, cp.wip
            ));
            continue;
        }
        // The course gave nothing: back to the castle state before it, and skip it for now.
        if o.beat > 0 {
            break Attempt::Gave("no shorter run".into());
        }
        log(format!("  {}; skipping {}, back to the castle ({:.0} s)", out.note, name(zone), t0.elapsed().as_secs_f64()));
        cp_n = 0;
        for l in std::iter::once(zone).chain(VIA.iter().filter(|v| v.1 == zone).map(|v| v.0)) {
            if !mem.skip.contains(&l) {
                mem.skip.push(l);
            }
        }
        match &hub {
            Some((g, n)) => {
                game.load_state(g)?;
                played.truncate(*n);
            }
            None => {
                // The block started in this course: out by the pause menu.
                if !pause_exit(&mut game, &mut played, max, &before)? {
                    break Attempt::Gave(format!("{} gave nothing and the pause-menu exit didn't reach the castle", name(zone)));
                }
                log(format!("  out of {} by the pause menu at frame {}", name(zone), played.len()));
            }
        }
    };
    mem.save(&mem_path, &origin);
    Ok(result)
}

trait GameView {
    fn game_view(&self) -> Result<SM64MiningGame>;
}

impl GameView for SM64MiningSave {
    /// A game in this save's state (to read it).
    fn game_view(&self) -> Result<SM64MiningGame> {
        let mut g = sm64pow::generator()?.create_game()?;
        g.load_state(&self.game)?;
        Ok(SM64MiningGame { game: g, pert: self.pert.clone() })
    }
}

/// Goal bias from the best run so far (the anytime miner's later searches): the
/// run's path as waypoint objectives for the tree (an `OBRRT_SPEC` file), as
/// RRT samples its goal at a fixed rate among random targets. Mario's position
/// every `every` frames in the run's courses (not the castle) becomes a spec
/// "reach this point" with pool weight `weight` (the bias: the sampler picks
/// objectives in proportion to their weights; kinship is waived). `pads` are the run's played pads
/// from the block's start. Returns how many waypoints were written.
pub fn write_guide(start: Start, parent: Option<&SaveState>, seed: &Seed, params: &Params, pads: &[GamePad], every: usize, weight: f64, path: &std::path::Path) -> Result<usize> {
    let (mut game, _) = sm64pow::chain::mining_start(start, parent, seed, params)?;
    let mut specs = Vec::new();
    for (f, p) in pads.iter().enumerate() {
        game.game.step_game(*p)?; // played pads: the pokes are in them already
        if f % every != every - 1 {
            continue;
        }
        let st = read_state(&game.game, (f + 1) as u32, false);
        if is_hub_level(st.level) || !st.controllable() {
            continue;
        }
        let pos = [st.pos[0].round(), st.pos[1].round(), st.pos[2].round()];
        specs.push(serde_json::json!({
            "name": format!("guide_{}", specs.len()),
            "zone": [st.level, st.area],
            "phases": [{ "target": pos, "done": { "in_sphere": { "center": pos, "r": 200 } } }],
            // Kinship waived: any state in the tree may start toward a waypoint (not only
            // b's near relatives), so the best-placed one is picked by its ETA.
            "hints": { "obj_weight": weight, "kinship": 1000 },
        }));
    }
    let n = specs.len();
    crate::sys::fs::write(path, serde_json::to_string_pretty(&serde_json::json!({ "specs": specs }))?)?;
    Ok(n)
}
