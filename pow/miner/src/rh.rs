//! The automated miner's controller built on the user's rolling-horizon GA
//! (objective-rrt-v2 `RhController`, the ETA objectives). Every worker game is an `SM64MiningGame` (the search crate's
//! `sm64coin` feature), so every rollout is perturbed exactly as the verifier
//! replays it. Deref trap guard: a shadow `SM64MiningGame` replays each
//! committed frame through `step` (the real perturbed step), the pads it
//! returns are the played pads sealed into the proof, and its position is
//! compared with the controller's at every commit (a controller that stepped
//! unperturbed would drift from the shadow at the first poke).
//!
//! Routes: each block is the game's next progress point, played on from the
//! parent block's end state, so the controller first looks at where the game
//! is and which stars the save file already has, and picks a [`Plan`] from the
//! library below (see [`plans`]): a list of spec legs (objective-rrt-v2
//! tools/spec/routes/*.json) followed as a sliding window of objectives.
//! Coverage today is BoB's two walkable stars; for any other state there is no
//! route ([`Attempt::NoRoute`]) and the caller waits (or, after the stall
//! time, starts from power-on).
use anyhow::{Result, bail};
use obrrt_core::{GamePad, Objective, St, buttons::{A_BUTTON, START_BUTTON}, read_state};
use obrrt_search::ga::rhstep::{RhConfig, RhController};
use serde_json::Value;
use sm64pow::proof::Start;
use sm64pow::sm64_binds::SaveState;
use sm64pow::{Params, SM64MiningGame, Seed};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use crate::sys::Instant;

const CHOMP: &str = include_str!("../routes/rh-bob-chomp.json");
const KING: &str = include_str!("../routes/rh-bob-king.json");
/// Leg keys that only the route runner (legs) reads.
const LEG_ONLY: &[&str] = &["doc", "dialogs", "reference", "budget", "stable_frames", "expect_frames", "require_next", "done_requires_next", "moves", "window_break"];

/// Castle-grounds, castle (any floor) and Bob-omb Battlefield level numbers
/// (`St::level`).
const LEVEL_GROUNDS: u16 = 16;
const LEVEL_CASTLE: u16 = 6;
const LEVEL_BOB: u16 = 9;

/// One route: legs in order, the star it ends with, and where each part of it
/// starts (so a state already past the entry legs skips them).
pub struct Plan {
    pub name: &'static str,
    /// The star it collects: (course 1-25, star 1-7 as in the star select).
    pub star: (i16, u8),
    pub legs: Vec<Arc<dyn Objective>>,
    /// First leg when Mario is in the castle (the BoB painting).
    pub from_castle: usize,
    /// First leg when Mario is already in BoB.
    pub from_bob: usize,
}

fn legs_of(doc: &Value, only: impl Fn(&str) -> bool) -> Result<Vec<(String, Arc<dyn Objective>)>> {
    let mut out = Vec::new();
    for (i, leg) in doc["legs"].as_array().ok_or_else(|| anyhow::anyhow!("route: no legs"))?.iter().enumerate() {
        let name = leg["name"].as_str().unwrap_or("").to_string();
        if !only(&name) {
            continue;
        }
        let mut m = leg.as_object().unwrap().clone();
        for k in LEG_ONLY {
            m.remove(*k);
        }
        let mut specs = obrrt_spec::Spec::parse(&Value::Object(m), &format!("$.legs[{i}]"), None)?;
        out.push((name, Arc::new(specs.pop().ok_or_else(|| anyhow::anyhow!("leg {i}: no spec"))?) as Arc<dyn Objective>));
    }
    Ok(out)
}

/// The route library, in the order the AutoMiner prefers (a plan applies when
/// its star isn't collected yet and Mario is in the grounds, the castle or BoB):
///
/// 1. BoB star 6, "Behind Chain Chomp's Gate" (bob-chomp.json): from power-on.
/// 2. BoB star 1, "Big Bob-omb on the Summit" (bob-king.json): the same entry
///    legs, then the walked mountain path and the three King throws.
///
/// Anything else (other stars, red coins, keys, courses beyond BoB) has no
/// route yet.
pub fn plans() -> Result<Vec<Plan>> {
    let chomp: Value = serde_json::from_str(CHOMP)?;
    let king: Value = serde_json::from_str(KING)?;
    let entry = ["Bridge", "BridgeEnd", "CastleDoor", "BobPainting"];
    let mut out = Vec::new();
    for (name, star, own) in [("bob-chomp", (1i16, 6u8), &chomp), ("bob-king", (1, 1), &king)] {
        let mut legs: Vec<Arc<dyn Objective>> = legs_of(&chomp, |n| entry.contains(&n))?.into_iter().map(|x| x.1).collect();
        let from_castle = legs.len() - 1; // BobPainting
        let from_bob = legs.len();
        let rest = legs_of(own, |n| !entry.contains(&n))?;
        legs.extend(rest.into_iter().map(|x| x.1));
        out.push(Plan { name, star, legs, from_castle, from_bob });
    }
    Ok(out)
}

/// The plan for a game in state `st` (stars read from the save file) and the
/// first leg to follow, or why there is none.
pub fn choose(plans: &[Plan], st: &St) -> std::result::Result<(usize, usize), String> {
    let first = match st.level {
        LEVEL_GROUNDS => |_: &Plan| 0,
        LEVEL_CASTLE => |p: &Plan| p.from_castle,
        LEVEL_BOB => |p: &Plan| p.from_bob,
        l => return Err(format!("Mario is in level {l} (not the castle grounds, the castle or BoB)")),
    };
    for (i, p) in plans.iter().enumerate() {
        if !st.has_star(p.star.0, p.star.1 - 1) {
            return Ok((i, first(p)));
        }
    }
    Err("every star the library has a route for is collected".into())
}

/// What the AutoMiner can make: (progress point, route).
pub fn coverage() -> Vec<(&'static str, &'static str)> {
    vec![
        ("star: BoB 6 Behind Chain Chomp's Gate", "bob-chomp (from power-on, the castle or BoB)"),
        ("star: BoB 1 Big Bob-omb on the Summit", "bob-king (from the castle or BoB, e.g. after the chomp star)"),
    ]
}

#[derive(Clone)]
pub struct RhOptions {
    pub pop: usize,
    pub horizon: usize,
    pub commit: usize,
    pub gens: usize,
    pub threads: usize,
    /// Objectives in the controller's window.
    pub window: usize,
    pub seed: u64,
    /// Frames on one objective before rewinding to its start.
    pub stall: usize,
    pub max_retries: u64,
    /// Frames a run may take (0: the most any difficulty allows). A run longer
    /// than the current time limit becomes valid later (continuous decay).
    pub frame_cap: usize,
}

impl Default for RhOptions {
    fn default() -> Self {
        RhOptions { pop: 32, horizon: 600, commit: 4, gens: 1, threads: 4, window: 3, seed: 1, stall: 900, max_retries: 40, frame_cap: 0 }
    }
}

/// A found run.
pub struct Mined {
    pub pads: Vec<GamePad>,
    pub sim_frames: u64,
    pub seconds: f64,
    /// rrt: the keys of the objectives that made the run's path (all stages), for the next search's guide
    /// (OBRRT_GUIDE_KEYS); empty for rh.
    pub keys: Vec<String>,
    /// rrt: the played length at the start of each campaign step (the run's segments, for sharing it).
    pub starts: Vec<usize>,
}

/// When the start has settled: Mario is controllable and has either stood
/// still for [`Calm::STILL`] frames or, because the perturbation pokes the pad
/// (so he may keep twitching), has stayed controllable for
/// [`Calm::CONTROLLABLE`] frames. Deterministic.
#[derive(Default)]
pub struct Calm {
    still: u32,
    controllable: u32,
}

impl Calm {
    pub const STILL: u32 = 10;
    pub const CONTROLLABLE: u32 = 60;
    pub fn step(&mut self, st: &St, before: [f32; 3]) {
        self.still = if st.pos == before { self.still + 1 } else { 0 };
        self.controllable = if st.controllable() { self.controllable + 1 } else { 0 };
    }
    pub fn ready(&self, st: &St) -> bool {
        st.controllable() && (self.still >= Self::STILL || self.controllable >= Self::CONTROLLABLE)
    }
}

/// Which macros run before the search: the state-reactive ones ([`crate::macros`], the default) or the
/// fixed schedules the miners shipped with ([`boot_legacy`], [`settle_legacy`]). Process-wide, set once at
/// start-up (automine `--legacy-macros`, the web miner's `legacy_macros` request field, or the environment
/// variable `SM64COIN_LEGACY_MACROS=1` natively). Either is just a different input sequence: a proof
/// carries the inputs, so nodes replay it like any other.
static LEGACY_MACROS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_legacy_macros(on: bool) {
    LEGACY_MACROS.store(on, Ordering::Relaxed);
}

pub fn legacy_macros() -> bool {
    LEGACY_MACROS.load(Ordering::Relaxed) || crate::sys::env::var("SM64COIN_LEGACY_MACROS").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// From power-on to a controllable Mario: [`crate::macros::boot`] (or the legacy fixed schedule, see
/// [`legacy_macros`]). Played through the perturbation like everything else; `played` gets every pad.
pub fn boot(game: &mut SM64MiningGame, played: &mut Vec<GamePad>, max_frames: usize) -> Result<St> {
    if legacy_macros() { boot_legacy(game, played, max_frames) } else { crate::macros::boot(game, played, max_frames) }
}

/// From a parent's end state (a star's dance, then the save menu) to a controllable Mario:
/// [`crate::macros::settle`] (or the legacy schedule, see [`legacy_macros`]).
pub fn settle(game: &mut SM64MiningGame, played: &mut Vec<GamePad>, max_frames: usize) -> Result<St> {
    if legacy_macros() { settle_legacy(game, played, max_frames) } else { crate::macros::settle(game, played, max_frames) }
}

/// The legacy boot, obrrt2's fixed one: A on alternate frames, Start at 150-160 and 200-300,
/// 1680 frames; then idle until Mario is controllable and has stood still for
/// 10 frames. Played through the perturbation like everything else.
pub fn boot_legacy(game: &mut SM64MiningGame, played: &mut Vec<GamePad>, max_frames: usize) -> Result<St> {
    for i in 0..1680 {
        let mut b = 0;
        if (150..160).contains(&i) || (200..300).contains(&i) {
            b = START_BUTTON;
        }
        if i % 2 == 0 {
            b = A_BUTTON;
        }
        played.push(game.step(GamePad::new(b, 0, 0))?.0);
    }
    let mut st = read_state(game, played.len() as u32, false);
    let mut calm = Calm::default();
    while !calm.ready(&st) {
        let before = st.pos;
        played.push(game.step(GamePad::default())?.0);
        st = read_state(game, played.len() as u32, false);
        calm.step(&st, before);
        if played.len() >= max_frames {
            bail!("boot: Mario didn't stand still");
        }
    }
    Ok(read_state(game, played.len() as u32, true))
}

/// A parent end state is usually the moment a star was grabbed, in the middle
/// of the star dance: the game exits the course to the castle and opens the
/// save menu ("save and continue"). Press A on alternate frames until Mario is
/// controllable (A confirms the menu's first option), then idle until he has
/// stood still for 10 frames, and read the state with its objects.
pub fn settle_legacy(game: &mut SM64MiningGame, played: &mut Vec<GamePad>, max_frames: usize) -> Result<St> {
    let mut st = read_state(game, played.len() as u32, false);
    let mut calm = Calm::default();
    // Frames in a row stuck in a level change with no menu (see below).
    let mut warp_wait = 0u32;
    while !calm.ready(&st) {
        let before = st.pos;
        // A confirms the menus and dialogs. The perturbation can move the save
        // menu's cursor so that A picks "save and quit": the game then waits
        // in a level change (play mode 4) on the title screen for Start.
        warp_wait = if st.play_mode != 0 && !st.modal() && !st.controllable() { warp_wait + 1 } else { 0 };
        let a = match (st.controllable(), played.len() % 2 == 0) {
            (true, _) | (_, false) => 0,
            _ if warp_wait >= 30 => START_BUTTON,
            _ => A_BUTTON,
        };
        played.push(game.step(GamePad::new(a, 0, 0))?.0);
        st = read_state(game, played.len() as u32, false);
        calm.step(&st, before);
        if played.len() >= max_frames {
            bail!("settle: Mario never stood still after the parent's end state");
        }
    }
    Ok(read_state(game, played.len() as u32, true))
}

/// The outcome of an attempt.
pub enum Attempt {
    /// A run that made the next progress point.
    Found(Mined),
    /// Stopped, out of frames or gave up (try again: another template, seed).
    Gave(String),
    /// The library has no route from this state (the caller should wait for
    /// another start, e.g. the power-on start after the stall time).
    NoRoute(String),
}

/// Mines one block from `start` (the parent's end state `parent` for
/// [`Start::Parent`]): plays on until the rules' success condition holds
/// (one more star/key than the start). The pads are those played on every
/// frame from the start, ready for `sm64pow::seal_proof_at`.
pub fn mine(start: Start, parent: Option<&SaveState>, seed: &Seed, params: &Params, o: &RhOptions, stop: &AtomicBool, mut log: impl FnMut(String)) -> Result<Attempt> {
    let t0 = Instant::now();
    let library = plans()?;
    let max = if o.frame_cap > 0 { o.frame_cap } else { sm64pow::rules().max_frames_any() as usize };
    let _ = params;
    let (mut game, before) = sm64pow::chain::mining_start(start, parent, seed, params)?;
    let mut played: Vec<GamePad> = Vec::new();
    let st = match start {
        Start::PowerOn => boot(&mut game, &mut played, max)?,
        Start::Parent => settle(&mut game, &mut played, max)?,
    };
    log(format!("start {:?} at {} stars {} keys, ready at frame {} (level {} stars {})", start, before.stars, before.keys, st.frame, st.level, st.stars));
    let (plan_i, first) = match choose(&library, &st) {
        Ok(x) => x,
        Err(why) => return Ok(Attempt::NoRoute(why)),
    };
    let plan = &library[plan_i];
    log(format!("route {} from leg {}/{}", plan.name, first, plan.legs.len()));
    let objs: Vec<Arc<dyn Objective>> = plan.legs[first..].to_vec();
    let cfg = RhConfig { pop: o.pop, h: o.horizon, k: o.commit, g: o.gens, seed: o.seed, ..Default::default() };
    // The workers are forks of `game`: mining games, perturbation included.
    let mut ctl = RhController::new(&crate::hook::hooked(&game)?, o.threads, &crate::hook::hooked_save(&game.save_state()), st.clone(), cfg.clone())?;
    // Rewind point: the state where the current objective became the front.
    let mut ckpt = (game.save_state(), st, played.len());
    let mut retries = 0u64;
    // The shadow: what the verifier replays.
    let mut shadow = game;
    let rules = sm64pow::rules();
    let mut next = 0usize; // first unmet objective
    let mut last_log = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(Attempt::Gave("stopped".into()));
        }
        if played.len() >= max {
            log("out of frames".into());
            return Ok(Attempt::Gave("out of frames".into()));
        }
        let stages: Vec<&dyn Objective> = objs[next..].iter().take(o.window).map(|a| a.as_ref()).collect();
        let out = ctl.step(&stages, None)?;
        let met = ctl.met();
        // Stuck (every plan dies or is lost) or stalled on one objective (e.g.
        // swimming in the moat): back to the objective's start, another salt.
        if (out.stuck && met == 0) || (played.len() - ckpt.2 > o.stall && met == 0) {
            retries += 1;
            if retries > o.max_retries {
                log(format!("giving up at frame {} on {}", ctl.state().frame, objs[next].key()));
                return Ok(Attempt::Gave(format!("gave up on {}", objs[next].key())));
            }
            log(format!("{} at frame {} on {}: rewind to frame {} (retry {retries})", if out.stuck { "stuck" } else { "stalled" }, ctl.state().frame, objs[next].key(), ckpt.2));
            shadow.load_state(&ckpt.0)?;
            played.truncate(ckpt.2);
            ctl.load(&crate::hook::hooked_save(&ckpt.0), ckpt.1.clone(), true)?;
            ctl.set_config(RhConfig { salt: retries, ..cfg.clone() });
            continue;
        }
        for p in &out.pads {
            played.push(shadow.step(*p)?.0);
            if rules.is_success(&before, &shadow.progress()?) {
                let seconds = t0.elapsed().as_secs_f64();
                return Ok(Attempt::Found(Mined { pads: played, seconds, sim_frames: ctl.frames, keys: Vec::new(), starts: Vec::new() }));
            }
        }
        // Guard: the controller's committed state must be the shadow's.
        let cs = ctl.state();
        let ss = read_state(&shadow, played.len() as u32, false);
        if cs.pos != ss.pos || cs.frame as usize != played.len() {
            bail!("controller diverged from the perturbed replay at frame {} (pos {:?} vs {:?}): a game was stepped unperturbed", played.len(), cs.pos, ss.pos);
        }
        if met > 0 {
            ctl.pop_front(met);
            next += met;
            ckpt = (shadow.save_state(), ctl.state().clone(), played.len());
            log(format!("objective {} met at frame {} ({:.0} s)", next, played.len(), t0.elapsed().as_secs_f64()));
            if next >= objs.len() {
                log("route done without the rules' success".into());
                return Ok(Attempt::Gave("route done without progress".into()));
            }
        }
        if last_log.elapsed().as_secs() >= 30 {
            last_log = Instant::now();
            log(format!("frame {} obj {}/{} eta {:.0} sim {} pos {:?} act {:#x}", played.len(), next, objs.len(), out.eta, ctl.frames, ctl.state().pos, ctl.state().action));
        }
    }
}
