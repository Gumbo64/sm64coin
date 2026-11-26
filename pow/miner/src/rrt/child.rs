//! One tree step of the RRT miner, run in its own process (`rrtrun`): the
//! user's objective-RRT engine (objective-rrt-v2, feature `hooked`) from a
//! start state, over perturbed games only.
//!
//! Why a process: the engine and the objectives read many `OBRRT_*` switches
//! once per process, so each step gets a fresh process, as the seg120
//! campaign runs one `obrrt2` per step. The switches are the campaign's
//! (`campaign::env`), read the way `obrrt2` reads them
//! (`obrrt_search::envconfig`).
//!
//! The start is a mining save: the game's memory plus the block's
//! perturbation state (the engine stores the perturbation in every node).
//! Every worker game is a fork of the mining game, so every rollout, copy,
//! sync, save and load is the perturbed one. The step returns the wanted pads
//! from the root to the state it was asked for (see [`Goal`]) or, when there
//! is none and the job asks, to a checkpoint chosen as campaign.py's
//! `cannon_cp`, `stage_cp` and `checkpoint` choose one. The caller replays the
//! pads through its own shadow `SM64MiningGame`, which is what the verifier
//! replays.
use anyhow::{Result, bail};
use obrrt_core::{St, check_layout, read_state};
use obrrt_objectives::Registry;
use obrrt_search::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sm64pow::SM64MiningGame;
use sm64pow::game::Perturber;
use sm64pow::sm64_binds::{GamePad, SaveState};
use std::collections::HashMap;
use std::sync::Arc;

/// The castle's levels (inside, grounds, courtyard).
pub const HUB: [u16; 3] = [6, 16, 26];

pub fn is_hub_level(level: u16) -> bool {
    HUB.contains(&level)
}

/// What the step looks for: the earliest state of the tree that is
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub enum Goal {
    /// in a course (not the castle): `target`'s level, or any level not in
    /// the job's skip list, in control (the castle's tree);
    Enter,
    /// past the root's progress (stars, keys, the grand star), with
    /// `need_flags` set in the save flags (a cap switch), in control;
    Progress,
    /// in the castle with `need_flags` set (the moat drained), in control.
    Flag,
}

/// The checkpoint the caller wants when there is no goal state (campaign.py
/// `checkpoint`'s arguments).
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct CpAsk {
    /// Checkpoints taken in a row so far.
    pub n: usize,
    /// The goal the last checkpoint was taken for: (its key, how).
    pub prefer: Option<(String, String)>,
    /// (level, pos) of this course's earlier checkpoints that led nowhere.
    pub avoid: Vec<(u16, [f32; 3])>,
    /// STAGE_KINDS already used as checkpoints in this course visit.
    pub used_stages: Vec<String>,
    pub keys: u8,
    /// The last search (Bowser 3's arena is a checkpoint worth having).
    pub end: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub rom: String,
    /// The start's game memory (raw `SaveState` bytes).
    pub state_file: String,
    pub pert: Perturber,
    /// Frames played since the block's start (the root's frame number).
    pub frame0: u32,
    pub goal: Goal,
    /// `Goal::Enter`: the course to enter (None: any course not skipped).
    pub target: Option<u16>,
    /// `Goal::Progress` / `Goal::Flag`: save flags the state must have.
    pub need_flags: u32,
    /// Levels the tree is told not to take (OBRRT_EXIT_SKIP).
    pub skip: Vec<u16>,
    /// The tree's settings (`OBRRT_*`), applied before anything reads them.
    pub env: Vec<(String, String)>,
    /// Emulated-frame budget of the tree.
    pub budget: u64,
    /// Wall-clock cap in seconds (0: none).
    pub seconds: f64,
    pub threads: usize,
    pub seed: u64,
    /// The path to the winner may not be longer than this many frames.
    pub max_path: u32,
    pub out_dir: String,
    pub checkpoint: Option<CpAsk>,
    /// The browser build: the tree can be sampled for the page's visualisation (see [`viz`]); the page
    /// switches the feed on and off while the search runs.
    #[serde(default)]
    pub viz: bool,
    /// Tree sync with the other workers of the block (the browser build, see [`tsync`]): every this many
    /// seconds (0: off) the search offers its best `sync_k` frontier states and takes the others' that
    /// carry the same `sync_tag` (same step input and same played prefix).
    #[serde(default)]
    pub sync_secs: f64,
    #[serde(default)]
    pub sync_k: usize,
    #[serde(default)]
    pub sync_tag: String,
    /// Paths from the root (prefixes of a known run's segment) added to the tree as boosted nodes before the
    /// search expands (a shared run: the search branches off it, see `Engine::insert_manual`), with their boost.
    #[serde(default)]
    pub manual: Vec<Vec<(u16, i8, i8)>>,
    #[serde(default)]
    pub manual_weight: f64,
}

/// The channel the step's process reaches the other workers by (set by the browser build before
/// `run_step`; natively there is none, the miner's threads share one tree).
pub static EXCHANGE: std::sync::OnceLock<super::Exchange> = std::sync::OnceLock::new();

/// What a step's tree sync did (see [`tsync`]).
#[derive(Default, Clone, Serialize, Deserialize, Debug)]
pub struct SyncStats {
    pub rounds: u32,
    pub offered: u32,
    pub received: u32,
    pub added: u32,
    pub dup: u32,
    pub known: u32,
    pub bad: u32,
    pub errors: u32,
    /// Frames spent replaying imports.
    pub frames: u64,
    /// Imported nodes that were later started from (expanded) / that the winner's path runs through.
    pub expanded: u32,
    pub on_path: u32,
}

/// A state to continue from (campaign.py's checkpoint record).
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
pub struct Cp {
    pub pads: Vec<(u16, i8, i8)>,
    pub node: u32,
    pub kind: String,
    pub key: Option<String>,
    pub how: String,
    pub frame: u32,
    pub level: u16,
    pub pos: [f32; 3],
    pub eta: f64,
    pub eta0: Option<f64>,
    pub wip: i64,
    pub wip0: i64,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct StepOut {
    pub found: bool,
    pub pads: Vec<(u16, i8, i8)>,
    /// What the winner is: its level, frame, stars, keys.
    pub level: u16,
    pub frame: u32,
    pub stars: i16,
    pub keys: u8,
    pub pos: [f32; 3],
    pub emulated: u64,
    pub nodes: usize,
    /// States that met the goal (any path length).
    pub exits: usize,
    pub note: String,
    pub checkpoint: Option<Cp>,
    /// The keys of the objectives whose attempts made the winner's path (root first; nodes
    /// without an attempt event, e.g. dense adds, are skipped), for the next search's guide.
    #[serde(default)]
    pub path_keys: Vec<String>,
    #[serde(default)]
    pub sync: Option<SyncStats>,
}

/// In control: not in the cutscene action group, controllable, no menu.
fn in_control(st: &St) -> bool {
    st.action & 0x1C0 != 0x100 && st.controllable() && !st.modal()
}

fn meets(job: &Job, root: &St, st: &St) -> bool {
    match job.goal {
        Goal::Enter => {
            !is_hub_level(st.level)
                && match job.target {
                    Some(t) => st.level == t,
                    None => !job.skip.contains(&st.level),
                }
                && in_control(st)
                // A castle star on the way also counts (it is the block's progress).
                || st.progress() > root.progress()
        }
        Goal::Progress => st.progress() > root.progress() && st.save_flags & job.need_flags == job.need_flags,
        Goal::Flag => is_hub_level(st.level) && st.save_flags & job.need_flags == job.need_flags && st.action & 0x1C0 != 0x100,
    }
}

/// The first round's plans in the browser (see `run_step`).
#[cfg(target_arch = "wasm32")]
const RAMP_FIRST: usize = 2;

/// Runs one tree step in this process. Call once per process.
pub fn run_step(job: &Job) -> Result<StepOut> {
    // Settings first: the objectives read them lazily, once.
    // SAFETY: single-threaded here (nothing else has started).
    unsafe {
        for (k, v) in &job.env {
            crate::sys::env::set_var(k, v);
        }
        crate::sys::env::set_var("RAYON_NUM_THREADS", job.threads.to_string());
        if job.skip.is_empty() {
            crate::sys::env::remove_var("OBRRT_EXIT_SKIP");
        } else {
            crate::sys::env::set_var("OBRRT_EXIT_SKIP", job.skip.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","));
        }
    }
    let (tech, names) = obrrt_objectives::setup::from_env().map_err(anyhow::Error::msg)?;
    println!("techniques ({tech}): {names:?}");
    rayon::ThreadPoolBuilder::new().num_threads(job.threads).build_global().ok();
    let mut cfg = obrrt_search::Config::default();
    obrrt_search::envconfig::from_env(&mut cfg)?;
    cfg.out_dir = job.out_dir.clone();
    cfg.seed = job.seed;
    cfg.minutes = if job.seconds > 0.0 { job.seconds / 60.0 } else { 1e9 };
    cfg.max_frames = Some(job.budget);
    crate::sys::fs::create_dir_all(&job.out_dir)?;

    sm64pow::init(crate::sys::fs::read(&job.rom)?)?;
    let mut g = sm64pow::generator()?.create_game()?;
    // Not kept: the viz sampler reads the file again whenever the page needs the step's start state.
    g.load_state(&SaveState::from_bytes(crate::sys::fs::read(&job.state_file)?)?)?;
    let game = SM64MiningGame { game: g, pert: job.pert.clone() };
    check_layout(&game)?;
    let root = read_state(&game, job.frame0, true);
    println!("root: frame {} level {} course {} area {} act_select {} act_init {} cursor {} action {:#x} play_mode {} pos {:?}; goal {:?} target {:?} need {:#x}", root.frame, root.level, root.course, root.area, root.act_select, root.act_init, root.act_cursor, root.action, root.play_mode, root.pos, job.goal, job.target, job.need_flags);
    obrrt_search::envconfig::after_root(&root);

    let generator = guide::wrap(obrrt_spec::wrap(Arc::new(Registry))?)?;
    let save = game.save_state();
    let mut engine = Engine::new_hooked(cfg, &crate::hook::hooked(&game)?, job.threads, Vec::new(), crate::hook::hooked_save(&save), root.clone(), generator.clone())?;
    if job.viz {
        engine.on_round = Some(viz::sampler(job, &root, generator.clone()));
    } else {
        #[cfg(not(target_arch = "wasm32"))]
        {
            engine.on_round = Some(viz::progress_file(format!("{}.progress", job.out_dir)));
        }
    }
    let sync_stats = Arc::new(std::sync::Mutex::new(SyncStats::default()));
    if job.sync_secs > 0.0 && job.sync_k > 0 && !job.sync_tag.is_empty() {
        if let Some(ex) = EXCHANGE.get() {
            engine.on_sync = Some(tsync::hook(job, ex.clone(), &root, sync_stats.clone()));
        }
    }
    // Hybrid mining (the page's human-played nodes, state requests and position goal): needs the step's tag.
    let mut page: Option<Box<dyn FnMut(&mut Engine) + Send>> = None;
    if !job.sync_tag.is_empty() {
        if let Some(ex) = EXCHANGE.get() {
            page = Some(hybrid::hook(job, ex.clone(), &root));
        }
    }
    // A shared run's prefixes (run sharing): added once as boosted manual nodes, before the page's hook.
    if !job.manual.is_empty() {
        let (segs, w) = (job.manual.clone(), job.manual_weight);
        let mut done = false;
        let mut page = page.take();
        engine.on_hybrid = Some(Box::new(move |e| {
            if !std::mem::replace(&mut done, true) {
                for s in &segs {
                    let _ = e.insert_manual(&pads_of(s), w);
                }
            }
            if let Some(f) = page.as_mut() {
                f(e);
            }
        }));
    } else {
        engine.on_hybrid = page;
    }
    // The browser's feed (and the page's first look at the search) comes after a round: with the full round's plans
    // the first came 20-30 s after Start (one wasm thread). So the first rounds are small and double up to the
    // configured size (2, 4, 8, 16 plans): same search, a first sample in a few seconds. Native runs keep their rounds.
    #[cfg(target_arch = "wasm32")]
    if job.viz && engine.cfg.round_plans > RAMP_FIRST {
        let full = engine.cfg.round_plans;
        let mut inner = engine.on_hybrid.take();
        engine.cfg.round_plans = RAMP_FIRST;
        engine.on_hybrid = Some(Box::new(move |e| {
            e.cfg.round_plans = (e.cfg.round_plans * 2).min(full);
            if let Some(f) = inner.as_mut() {
                f(e);
            }
        }));
    }
    engine.run()?;
    let sync_out = (job.sync_secs > 0.0 && engine.on_sync.is_some()).then(|| {
        let mut s = sync_stats.lock().unwrap().clone();
        s.expanded = engine.tree.nodes.iter().filter(|n| n.via == "sync" && n.stats.as_start > 0).count() as u32;
        s
    });

    let tree = &engine.tree;
    let mut best: Option<usize> = None;
    let mut exits = 0;
    for (i, n) in tree.nodes.iter().enumerate().skip(1) {
        if !meets(job, &root, &n.st) {
            continue;
        }
        exits += 1;
        if n.st.frame.saturating_sub(job.frame0) > job.max_path {
            continue;
        }
        if best.is_none_or(|b| n.st.frame < tree.nodes[b].st.frame) {
            best = Some(i);
        }
    }
    let mut out = StepOut { emulated: engine.total_frames_emulated(), nodes: tree.nodes.len(), exits, sync: sync_out, ..Default::default() };
    let Some(b) = best else {
        out.note = format!("no state with the goal in {} frames, {} nodes", out.emulated, out.nodes);
        if let Some(ask) = &job.checkpoint {
            out.checkpoint = choose_checkpoint(&engine, &root, ask, &job.out_dir)?;
            if let Some(c) = &mut out.checkpoint {
                c.pads = path(tree, c.node);
                if c.pads.len() as u32 != c.frame - job.frame0 {
                    bail!("checkpoint path has {} pads but the node is {} frames from the root", c.pads.len(), c.frame - job.frame0);
                }
            }
        }
        return Ok(out);
    };
    out.pads = path(tree, b as u32);
    let n = &tree.nodes[b];
    if out.pads.len() as u32 != n.st.frame - job.frame0 {
        bail!("tree path has {} pads but the winner is {} frames from the root", out.pads.len(), n.st.frame - job.frame0);
    }
    out.found = true;
    out.level = n.st.level;
    out.frame = n.st.frame;
    out.stars = n.st.stars;
    out.keys = n.st.keys;
    out.pos = n.st.pos;
    out.path_keys = path_keys(tree, b as u32, &job.out_dir);
    if let Some(sy) = out.sync.as_mut() {
        let mut at = Some(b as u32);
        while let Some(n) = at {
            sy.on_path += (tree.nodes[n as usize].via == "sync") as u32;
            at = tree.nodes[n as usize].parent;
        }
    }
    Ok(out)
}

/// The objective keys along the path to node `i`, from the run's events.jsonl.
fn path_keys(tree: &obrrt_search::tree::Tree, i: u32, out_dir: &str) -> Vec<String> {
    let mut made: HashMap<u64, String> = HashMap::new();
    if let Ok(text) = crate::sys::fs::read_to_string(std::path::Path::new(out_dir).join("events.jsonl")) {
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if let (Some(n), Some(k)) = (v["added"].as_u64(), v["key"].as_str()) {
                made.insert(n, k.to_string());
            }
        }
    }
    let mut chain = Vec::new();
    let mut at = Some(i);
    while let Some(n) = at {
        chain.push(n);
        at = tree.nodes[n as usize].parent;
    }
    chain.iter().rev().filter_map(|n| made.get(&(*n as u64)).cloned()).collect()
}

/// Goal bias from the best run so far: `OBRRT_GUIDE_KEYS=<file>` (one objective key per
/// line, from the run's `path_keys`) and `OBRRT_GUIDE_WEIGHT` (×, default 20). Every generated
/// objective with one of those keys gets its pool weight × the weight and its kinship waived
/// (any state in the tree may start it, not only near relatives of the state it was generated
/// from), as RRT samples its goal at a fixed rate among random targets.
mod guide {
    use anyhow::Result;
    use obrrt_core::{Arrival, Budget, DenseKeep, Eta, GaBudget, Generator, Mem, Objective, PadOverlay, Precision, St};
    use std::collections::HashSet;
    use std::sync::Arc;

    struct Guided {
        inner: Arc<dyn Objective>,
        weight: f64,
    }

    impl Objective for Guided {
        fn kind(&self) -> &'static str { self.inner.kind() }
        fn key(&self) -> String { self.inner.key() }
        fn obj_weight(&self, b: &St) -> f64 { self.inner.obj_weight(b) * self.weight }
        fn base_criteria(&self, s: &St) -> bool { self.inner.base_criteria(s) }
        fn eta(&self, s: &St, start: &St, mem: &mut Mem) -> Eta { self.inner.eta(s, start, mem) }
        fn fixed(&self) -> Option<Budget> { self.inner.fixed() }
        fn ga_budget(&self) -> Option<GaBudget> { self.inner.ga_budget() }
        fn precision(&self) -> Precision { self.inner.precision() }
        fn kinship(&self) -> u32 { 1000 }
        fn dense_keep(&self) -> DenseKeep { self.inner.dense_keep() }
        fn start_bias(&self, s: &St) -> f64 { self.inner.start_bias(s) }
        fn done_frames(&self, s: &St, mem: &Mem) -> f64 { self.inner.done_frames(s, mem) }
        fn temperature(&self) -> f64 { self.inner.temperature() }
        fn needs_objects(&self) -> bool { self.inner.needs_objects() }
        fn steer_target(&self, s: &St) -> Option<[f32; 3]> { self.inner.steer_target(s) }
        fn success_value(&self) -> f64 { self.inner.success_value() }
        fn diag_tag(&self) -> Option<String> { self.inner.diag_tag() }
        fn note_result(&self, success: bool, frames: usize, end_fv: Option<f32>) { self.inner.note_result(success, frames, end_fv) }
        fn steer_button(&self, s: &St) -> u16 { self.inner.steer_button(s) }
        fn arrival(&self, s: &St) -> Arrival { self.inner.arrival(s) }
        fn pad_overlay(&self) -> Option<PadOverlay> { self.inner.pad_overlay() }
        fn modal(&self) -> bool { self.inner.modal() }
        fn exists_in(&self, s: &St) -> bool { self.inner.exists_in(s) }
    }

    struct WithGuide {
        inner: Arc<dyn Generator>,
        keys: HashSet<String>,
        weight: f64,
    }

    impl Generator for WithGuide {
        fn generate(&self, b: &St, rng: &mut rand::rngs::SmallRng) -> Vec<Arc<dyn Objective>> {
            self.inner
                .generate(b, rng)
                .into_iter()
                .map(|o| if self.keys.contains(&o.key()) { Arc::new(Guided { inner: o, weight: self.weight }) as Arc<dyn Objective> } else { o })
                .collect()
        }
        fn wants_records(&self) -> bool { self.inner.wants_records() }
        fn summary_blocks(&self) -> Vec<(String, String)> {
            let mut v = self.inner.summary_blocks();
            v.push(("guide".into(), format!("{{\"keys\": {}, \"weight\": {}}}", self.keys.len(), self.weight)));
            v
        }
        fn report(&self, out_dir: &str) { self.inner.report(out_dir) }
    }

    pub fn wrap(inner: Arc<dyn Generator>) -> Result<Arc<dyn Generator>> {
        let Ok(path) = crate::sys::env::var("OBRRT_GUIDE_KEYS") else { return Ok(inner) };
        let keys: HashSet<String> = crate::sys::fs::read_to_string(&path)?.lines().filter(|l| !l.is_empty()).map(String::from).collect();
        let weight = crate::sys::env::var("OBRRT_GUIDE_WEIGHT").ok().map(|w| w.parse()).transpose()?.unwrap_or(20.0);
        println!("guide: {} objective keys from the best run, weight x{weight}, kinship waived", keys.len());
        Ok(Arc::new(WithGuide { inner, keys, weight }))
    }
}

/// The wanted pads from the root to node `i`.
fn path(tree: &obrrt_search::tree::Tree, i: u32) -> Vec<(u16, i8, i8)> {
    let mut chain = Vec::new();
    let mut at = Some(i);
    while let Some(i) = at {
        chain.push(i);
        at = tree.nodes[i as usize].parent;
    }
    let mut pads = Vec::new();
    for &i in chain.iter().rev() {
        for p in &tree.nodes[i as usize].inputs {
            pads.push((p.button, p.stick_x, p.stick_y));
        }
    }
    pads
}

pub fn pads_of(v: &[(u16, i8, i8)]) -> Vec<GamePad> {
    v.iter().map(|&(b, x, y)| GamePad::new(b, x, y)).collect()
}

fn near(avoid: &[(u16, [f32; 3])], level: u16, pos: [f32; 3]) -> bool {
    avoid.iter().any(|(l, q)| *l == level && (0..3).map(|k| (pos[k] - q[k]).powi(2)).sum::<f32>() < 300.0f32.powi(2))
}

/// Objective kinds that are boss fights (their wip is the boss's lost health).
const BOSS_KINDS: [&str; 11] = ["Eyerok", "BigBoo", "Bowser", "Wiggler", "WhompKing", "KingBobomb", "BigBully", "Bully", "MrI", "Chuckya", "Klepto"];

/// One-shot steps of a star's chain whose success is worth restarting from.
const STAGE_KINDS: [&str; 9] = ["Hmc::MetalSwitch", "Pyramid::TopHole", "WarpFloor::Enter", "PenguinRace::Win", "Snowman::LeadBody", "WdwWater::Slab", "MrI::Box", "MrI::Climb", "MrI::Gate"];

/// campaign.py: `cannon_cp`, then `stage_cp`, then `checkpoint`.
fn choose_checkpoint(engine: &Engine, root: &St, ask: &CpAsk, out_dir: &str) -> Result<Option<Cp>> {
    let tree = &engine.tree;
    let zone = root.level;
    // cannon_opened: the earliest state that opened a cannon the root hadn't,
    // grounded, idle or walking, with health.
    let cannons = |st: &St| -> u32 { (0..25).filter(|&c| st.course_stars[c] & 0x80 != 0).fold(0u32, |m, c| m | 1 << c) };
    let have = cannons(root);
    let mut best: Option<usize> = None;
    for (i, n) in tree.nodes.iter().enumerate().skip(1) {
        if cannons(&n.st) & !have == 0 || ![0x0c400201, 0x04000440, 0x0c400202].contains(&n.st.action) || n.st.health < 0x500 {
            continue;
        }
        if best.is_none_or(|b| n.st.frame < tree.nodes[b].st.frame) {
            best = Some(i);
        }
    }
    if let Some(b) = best {
        let st = &tree.nodes[b].st;
        return Ok(Some(Cp { node: b as u32, kind: "Cannon::Opened".into(), how: "moved".into(), frame: st.frame, level: st.level, pos: st.pos, ..Default::default() }));
    }
    // stage_cp: the earliest healthy state a STAGE_KINDS success added.
    let mut added: HashMap<u32, String> = HashMap::new();
    if let Ok(text) = crate::sys::fs::read_to_string(format!("{out_dir}/events.jsonl")) {
        for l in text.lines() {
            if !STAGE_KINDS.iter().any(|k| l.contains(k)) {
                continue;
            }
            let Ok(e) = serde_json::from_str::<Value>(l) else { continue };
            let kind = e["kind"].as_str().unwrap_or("");
            if STAGE_KINDS.contains(&kind) && !ask.used_stages.iter().any(|u| u == kind) && e["success"].as_bool() == Some(true) {
                let a = match &e["added"] {
                    Value::Array(v) => v.clone(),
                    v => vec![v.clone()],
                };
                for n in a {
                    if let Some(n) = n.as_u64() {
                        added.insert(n as u32, kind.to_string());
                    }
                }
            }
        }
    }
    let mut best: Option<u32> = None;
    for (&i, _) in &added {
        let Some(n) = tree.nodes.get(i as usize) else { continue };
        let st = &n.st;
        if st.frame <= root.frame || st.level != zone {
            continue;
        }
        if ![0x000, 0x040, 0x0C0].contains(&(st.action & 0x1C0)) || st.action & 0x20000 != 0 || st.health < 0x500 || near(&ask.avoid, st.level, st.pos) {
            continue;
        }
        if best.is_none_or(|b| st.frame < tree.nodes[b as usize].st.frame) {
            best = Some(i);
        }
    }
    if let Some(b) = best {
        let st = &tree.nodes[b as usize].st;
        return Ok(Some(Cp { node: b, kind: added[&b].clone(), how: "stage".into(), frame: st.frame, level: st.level, pos: st.pos, ..Default::default() }));
    }
    // checkpoint: from summary.json's goals.
    let summary: Value = match crate::sys::fs::read_to_string(format!("{out_dir}/summary.json")).ok().and_then(|t| serde_json::from_str(&t).ok()) {
        Some(v) => v,
        None => return Ok(None),
    };
    let goals = summary["goals"].as_array().cloned().unwrap_or_default();
    type Key = Vec<f64>;
    let mut m: Option<(Key, Cp)> = None;
    let mut e_best: Option<(Key, Cp)> = None;
    let mut w_best: Option<(Key, Cp)> = None;
    let mut b_best: Option<(Key, Cp)> = None;
    let mut p: Option<(Key, Cp)> = None;
    let better = |slot: &Option<(Key, Cp)>, k: &Key| slot.as_ref().is_none_or(|(s, _)| k.partial_cmp(s) == Some(std::cmp::Ordering::Greater));
    for g in &goals {
        let kind = g["kind"].as_str().unwrap_or("").to_string();
        if ask.n >= 1 && kind.starts_with("Tech::") {
            continue;
        }
        let cps: Vec<&Value> = match g["cps"].as_array() {
            Some(v) if !v.is_empty() => v.iter().collect(),
            _ => vec![g],
        };
        let cps: Vec<&Value> = cps
            .into_iter()
            .filter(|c| c["frame"].as_u64().unwrap_or(0) as u32 > root.frame)
            .filter(|c| !near(&ask.avoid, c["level"].as_u64().unwrap_or(0) as u16, pos_of(c)))
            .collect();
        if cps.is_empty() {
            continue;
        }
        let base = g["eta0"].as_f64().or_else(|| g["eta1"].as_f64());
        let weight = g["weight"].as_f64().unwrap_or(0.0);
        let wip0 = g["wip0"].as_i64().unwrap_or(0);
        let key = g["key"].as_str().map(String::from);
        let flat = |c: &Value, how: &str| Cp {
            pads: Vec::new(),
            node: c["node"].as_u64().unwrap_or(0) as u32,
            kind: kind.clone(),
            key: key.clone(),
            how: how.into(),
            frame: c["frame"].as_u64().unwrap_or(0) as u32,
            level: c["level"].as_u64().unwrap_or(0) as u16,
            pos: pos_of(c),
            eta: c["eta"].as_f64().unwrap_or(f64::INFINITY),
            eta0: base,
            wip: c["wip"].as_i64().unwrap_or(0),
            wip0,
        };
        let eta = |c: &Value| c["eta"].as_f64().unwrap_or(f64::INFINITY);
        let wip = |c: &Value| c["wip"].as_i64().unwrap_or(0);
        for c in &cps {
            let lvl = c["level"].as_u64().unwrap_or(0) as u16;
            // Only an arena still worth a fight (its key not had; Bowser 3 only at the end).
            let worth = match lvl {
                30 => ask.keys < 1,
                33 => ask.keys < 2,
                34 => ask.end,
                _ => true,
            };
            if lvl != zone && worth {
                let k = vec![weight, wip(c) as f64, -eta(c)];
                if better(&m, &k) {
                    m = Some((k, flat(c, "moved")));
                }
            }
        }
        let mut e = None;
        let mut w = None;
        let emin = cps.iter().map(|c| eta(c)).fold(f64::INFINITY, f64::min);
        if let Some(base) = base {
            if base - emin > (100.0f64).max(0.05 * base) {
                let c = cps
                    .iter()
                    .filter(|c| eta(c) <= emin + 0.1 * (base - emin))
                    .max_by(|a, b| (wip(a), -eta(a)).partial_cmp(&(wip(b), -eta(b))).unwrap_or(std::cmp::Ordering::Equal))
                    .unwrap();
                let k = vec![weight * (base - emin)];
                let x = (k.clone(), flat(c, "eta"));
                if better(&e_best, &k) {
                    e_best = Some(x.clone());
                }
                e = Some(x);
            }
        }
        let c = cps.iter().max_by(|a, b| (wip(a), -eta(a)).partial_cmp(&(wip(b), -eta(b))).unwrap_or(std::cmp::Ordering::Equal)).unwrap();
        if wip(c) > wip0 {
            let k = vec![wip(c) as f64, weight, -eta(c)];
            let x = (k.clone(), flat(c, "wip"));
            if better(&w_best, &k) {
                w_best = Some(x.clone());
            }
            if BOSS_KINDS.contains(&kind.split("::").next().unwrap_or("")) && better(&b_best, &k) {
                b_best = Some(x.clone());
            }
            w = Some(x);
        }
        if let Some((pk, how)) = &ask.prefer {
            if key.as_deref() == Some(pk.as_str()) {
                p = if how == "eta" { e.clone().or(w.clone()) } else { w.clone().or(e.clone()) };
            }
        }
    }
    let order = if ask.n % 2 == 0 { [m, b_best, p, e_best, w_best] } else { [m, b_best, p, w_best, e_best] };
    Ok(order.into_iter().flatten().next().map(|x| x.1))
}

fn pos_of(c: &Value) -> [f32; 3] {
    let p = |k: usize| c["pos"][k].as_f64().unwrap_or(0.0) as f32;
    [p(0), p(1), p(2)]
}

/// The tree sampled for the web miner's page (`Job::viz`): about once a second the closure of
/// [`viz::sampler`] hands `sm64miner.viz` a JSON line (wasm only; natively there is no viewer):
///
/// ```text
/// { "step": {"frame0", "seed", "goal", "level", "area", "pos": [x,y,z]},   // the step's root
///   "t": seconds since the step began, "nodes_total", "sim_frames",
///   "best": {"id", "frame" (from the step's start), "stars", "keys", "pos"},  // most progress, then soonest
///   "nodes": [[id, parent, frame (from the step's start), x, y, z, stars, level, area, flags]],
///   // up to NODES new nodes since the last sample (evenly thinned; flags bit 0: more progress than the root);
///   // "cov": {"places": distinct floors this tree reached in the step (see [`Reel`]), "records": records broken so far
///   // (a node 10+ frames sooner than any earlier one to a 256-unit cell), "new": the hashed keys of the floors new since the last line}
///   "pads": [button, stickX, stickY, ...]   // only when `best` changed: its inputs from the step's start
///   "clip": {"kind": "place"|"record", "id", "frame" (from the step's start), "gain" (record: frames saved), "pos", "level", "area"}
///   // a highlight (see [`Reel`]): at most one per ~2 s; the node's whole path is in the edges sent just before this line
/// }
/// ```
///
/// Before each line the sampler hands `sm64miner.viz_edges(key, bytes)` the tree edges the page's mirror lacks (every sampled
/// node's ancestors, each edge once per step; format at [`Edges`]), so the page can rebuild any sampled node's path itself.
pub mod viz {
    use super::*;
    use crate::sys::Instant;
    use obrrt_search::engine::Engine;

    const NODES: usize = 500;

    /// The step's start state (the SaveState bytes the step loaded, read again from `job.state_file`: not held while the
    /// feed is off) is offered to the page at the first sample with the feed on, and again at the first sample after the
    /// page switched the feed off (`sm64miner.viz_on()` false once: the page reports an off→on toggle that way), with
    /// the edges (resent from scratch: the page's mirror was dropped) and the best node's inputs
    /// (`sm64miner.viz_state(key, bytes)`, key = the viz feed's step key) so the page can start the game at the step's
    /// root and replay only the step's own inputs. The page can also ask for the inputs of a sampled node of this tree
    /// (`sm64miner.viz_req(key)` -> JSON ids; answered with `viz_path(key, id, flat pads)`; page-side only on cross-origin isolated pages).
    pub fn sampler(job: &Job, root: &St, _generator: Arc<dyn obrrt_core::Generator>) -> Box<dyn FnMut(&Engine) + Send> {
        let key = format!("{}:{}:{}:{}:{:?}", job.seed, job.frame0, root.level, root.area, job.goal);
        let key_str = key.clone();
        let mut fresh = true; // the page has none of this step's feed yet (first sample, or the feed was switched off)
        let head = serde_json::json!({ "frame0": job.frame0, "seed": job.seed, "goal": format!("{:?}", job.goal), "level": root.level, "area": root.area, "pos": root.pos });
        let job2 = job.clone();
        let (frame0, root_prog) = (job.frame0, root.progress());
        let mut last = Instant::now();
        let t0 = Instant::now();
        let mut seen = 1usize; // nodes below this index were sampled (the root isn't a sample)
        let mut best: Option<usize> = None;
        let mut reel = Reel::new(frame0, root);
        let mut edges = Edges::default();
        Box::new(move |e: &Engine| {
            // The page can switch the feed off while the search runs: then this is the whole cost.
            if last.elapsed().as_secs_f64() < 1.0 {
                return;
            }
            // Feed off: only a tiny progress line (frames simulated so far in this step), about once a second,
            // for the page's frames/s readout.
            if !viz_on() {
                fresh = true;
                last = Instant::now();
                emit(&format!("{{\"progress\":1,\"sim_frames\":{}}}", e.total_frames_emulated()));
                return;
            }
            last = Instant::now();
            let refresh = std::mem::take(&mut fresh);
            if refresh {
                if let Ok(st) = crate::sys::fs::read(&job2.state_file) {
                    send_state(&key, &st);
                    // With it the page can rebuild any node's exact state itself (hybrid mining: the root's perturbation
                    // and the step's sync tag; the node's path comes from the page's tree mirror).
                    if let Ok(p) = serde_json::to_string(&job2.pert) {
                        send_pert(&key, &serde_json::json!({ "tag": job2.sync_tag, "pert": p }).to_string());
                    }
                }
                edges = Edges::default();
            }
            for id in requested(&key) {
                if (id as usize) < e.tree.nodes.len() {
                    let flat: Vec<i32> = e.tree.path(id).iter().flat_map(|p| [p.button as i32, p.stick_x as i32, p.stick_y as i32]).collect();
                    send_path(&key, id, &flat);
                }
            }
            let nodes = &e.tree.nodes;
            let n = nodes.len();
            reel.scan(nodes, n);
            let key = |i: usize| (nodes[i].st.progress(), std::cmp::Reverse(nodes[i].st.frame));
            let mut changed = refresh;
            for i in seen..n {
                if best.is_none_or(|b| key(i) > key(b)) {
                    best = Some(i);
                    changed = true;
                }
            }
            let step = ((n - seen) / NODES).max(0) + 1;
            let sample: Vec<serde_json::Value> = (seen..n)
                .step_by(step)
                .map(|i| {
                    let nd = &nodes[i];
                    let s = &nd.st;
                    serde_json::json!([i, nd.parent, s.frame.saturating_sub(frame0), s.pos[0], s.pos[1], s.pos[2], s.stars, s.level, s.area, (s.progress() > root_prog) as u8])
                })
                .collect();
            for i in (seen..n).step_by(step) {
                edges.add(nodes, i as u32, false);
            }
            seen = n;
            let clip = reel.take();
            let fresh: Vec<u64> = std::mem::take(&mut reel.fresh);
            let mut msg = serde_json::json!({ "step": head, "t": t0.elapsed().as_secs_f64(), "nodes_total": n, "sim_frames": e.total_frames_emulated(), "nodes": sample });
            // Coverage: the distinct floors this tree reached in the step, the records broken, and the floors that are new.
            msg["cov"] = serde_json::json!({ "places": reel.places.len(), "records": reel.records, "new": fresh });
            if let Some(c) = clip {
                edges.add(nodes, c.id, true);
                msg["clip"] = c.json();
            }
            if !edges.buf.is_empty() {
                send_edges(&key_str, &edges.buf);
                edges.buf.clear();
            }
            if let Some(b) = best {
                let s = &nodes[b].st;
                msg["best"] = serde_json::json!({ "id": b, "frame": s.frame.saturating_sub(frame0), "stars": s.stars, "keys": s.keys, "pos": s.pos });
                if changed {
                    let pads: Vec<i64> = e.tree.path(b as u32).iter().flat_map(|p| [p.button as i64, p.stick_x as i64, p.stick_y as i64]).collect();
                    msg["pads"] = serde_json::json!(pads);
                }
            }
            emit(&msg.to_string());
        })
    }


    type Nodes = [obrrt_search::tree::Node];

    /// The highlight reel's detector. Over the nodes the search adds (scanned at each sample) it notices
    ///  * a "new place": a node standing on a floor no earlier node of this step stood on. A floor is its (level, area,
    ///    normal quantised to 0.05, plane offset n.p quantised to 50 units, moving object or none), so coplanar triangles
    ///    count as one place;
    ///  * a "record": a node that reached a 256-unit cell ([`obrrt_core::records::CELL`], the engine's own earliest-arrival
    ///    grid) at least 10 frames sooner than any earlier node of the step.
    /// At most one event leaves per 2 s (a new place beats a record; later places and bigger gains beat smaller): the best
    /// of the window. Events before frame 45 of the step are ignored (the step's own first moves).
    struct Reel {
        frame0: u32,
        scanned: usize,
        places: std::collections::HashSet<(u16, i16, [i8; 3], i32, i32)>,
        cells: HashMap<(u16, i16, [i32; 3]), u32>,
        pending: Option<Clip>,
        last: Instant,
        /// Coverage for the page's "Places reached" chart: new floors since the last sample (hashed keys), records broken so far.
        fresh: Vec<u64>,
        records: u32,
    }

    struct Clip {
        id: u32,
        place: bool,
        gain: u32,
        frame: u32,
        pos: [f32; 3],
        level: u16,
        area: i16,
    }

    impl Clip {
        fn score(&self) -> u64 {
            if self.place { 1_000_000 + self.frame as u64 } else { self.gain as u64 }
        }
        fn json(&self) -> Value {
            serde_json::json!({ "kind": if self.place { "place" } else { "record" }, "id": self.id, "frame": self.frame, "gain": self.gain, "pos": self.pos, "level": self.level, "area": self.area })
        }
    }

    const REEL_MIN_FRAME: u32 = 45;
    const REEL_MIN_GAIN: u32 = 10;
    const REEL_EVERY_S: f64 = 2.0;

    /// A place key as a number the page can keep in a set (52 bits: exact in a JavaScript number; the same in every worker).
    fn place_hash(k: &(u16, i16, [i8; 3], i32, i32)) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        k.hash(&mut h);
        h.finish() & ((1u64 << 52) - 1)
    }

    /// A node's floor as a place key; None when it is airborne or over no floor.
    fn place_key(s: &St) -> Option<(u16, i16, [i8; 3], i32, i32)> {
        let air = s.action & 0x1C0 == 0x080;
        let n = s.floor_normal;
        if air || n == [0.0; 3] || s.floor_y < -10000.0 || (s.pos[1] - s.floor_y).abs() > 20.0 {
            return None;
        }
        let q = |v: f32| (v * 20.0).round() as i8;
        let obj = s.floor_obj.map_or(-1, |o| o as i32);
        let off = if obj >= 0 { 0 } else { ((n[0] * s.pos[0] + n[1] * s.floor_y + n[2] * s.pos[2]) / 50.0).round() as i32 };
        Some((s.level as u16, s.area as i16, [q(n[0]), q(n[1]), q(n[2])], off, obj))
    }

    impl Reel {
        fn new(frame0: u32, root: &St) -> Reel {
            let mut r = Reel { frame0, scanned: 1, places: Default::default(), cells: HashMap::new(), pending: None, last: Instant::now(), fresh: Vec::new(), records: 0 };
            if let Some(k) = place_key(root) {
                r.places.insert(k);
                r.fresh.push(place_hash(&k));
            }
            r.cells.insert(Self::cell(root), 0);
            r
        }

        fn cell(s: &St) -> (u16, i16, [i32; 3]) {
            let c = |v: f32| (v / obrrt_core::records::CELL).floor() as i32;
            (s.level as u16, s.area as i16, [c(s.pos[0]), c(s.pos[1]), c(s.pos[2])])
        }

        fn scan(&mut self, nodes: &Nodes, n: usize) {
            for i in self.scanned..n {
                let s = &nodes[i].st;
                if !s.pos.iter().all(|v| v.is_finite()) {
                    continue;
                }
                let fr = s.frame.saturating_sub(self.frame0);
                let mut ev: Option<Clip> = None;
                let mk = |place: bool, gain: u32| Clip { id: i as u32, place, gain, frame: fr, pos: s.pos, level: s.level as u16, area: s.area as i16 };
                if let Some(k) = place_key(s) {
                    if self.places.insert(k) {
                        self.fresh.push(place_hash(&k));
                        if fr >= REEL_MIN_FRAME {
                            ev = Some(mk(true, 0));
                        }
                    }
                }
                let slot = self.cells.entry(Self::cell(s)).or_insert(u32::MAX);
                if *slot != u32::MAX && fr >= REEL_MIN_FRAME && fr + REEL_MIN_GAIN <= *slot {
                    self.records += 1;
                    if ev.is_none() {
                        ev = Some(mk(false, *slot - fr));
                    }
                }
                *slot = (*slot).min(fr);
                if let Some(c) = ev {
                    if self.pending.as_ref().is_none_or(|p| c.score() > p.score()) {
                        self.pending = Some(c);
                    }
                }
            }
            self.scanned = n;
        }

        /// The window's best event, once 2 s have passed since the last one.
        fn take(&mut self) -> Option<Clip> {
            if self.pending.is_some() && self.last.elapsed().as_secs_f64() >= REEL_EVERY_S {
                self.last = Instant::now();
                return self.pending.take();
            }
            None
        }
    }

    /// The tree edges the page's mirror lacks, as bytes (`sm64miner.viz_edges(key, bytes)`): per edge
    /// `u32 id, u32 parent (u32::MAX: none), u16 runs`, then per run `u8 count, u16 button, i8 stickX, i8 stickY`
    /// (the edge's pads, run-length coded; little endian). Parents precede children. An edge goes out once per step; past
    /// [`EDGE_CAP`] bytes only the highlights' ancestors are still sent (the page asks the worker for any other node).
    #[derive(Default)]
    struct Edges {
        sent: std::collections::HashSet<u32>,
        bytes: usize,
        buf: Vec<u8>,
    }

    const EDGE_CAP: usize = 6 << 20;

    impl Edges {
        fn add(&mut self, nodes: &Nodes, id: u32, force: bool) {
            if !force && self.bytes > EDGE_CAP {
                return;
            }
            let mut chain = Vec::new();
            let mut at = Some(id);
            while let Some(a) = at {
                if a == 0 || self.sent.contains(&a) {
                    break;
                }
                chain.push(a);
                at = nodes[a as usize].parent;
            }
            for &a in chain.iter().rev() {
                self.sent.insert(a);
                let nd = &nodes[a as usize];
                let start = self.buf.len();
                self.buf.extend_from_slice(&a.to_le_bytes());
                self.buf.extend_from_slice(&nd.parent.unwrap_or(u32::MAX).to_le_bytes());
                let runs_at = self.buf.len();
                self.buf.extend_from_slice(&[0, 0]);
                let mut runs = 0u16;
                let mut i = 0;
                while i < nd.inputs.len() {
                    let p = &nd.inputs[i];
                    let mut c = 1;
                    while i + c < nd.inputs.len() && c < 255 && (nd.inputs[i + c].button, nd.inputs[i + c].stick_x, nd.inputs[i + c].stick_y) == (p.button, p.stick_x, p.stick_y) {
                        c += 1;
                    }
                    self.buf.push(c as u8);
                    self.buf.extend_from_slice(&(p.button as u16).to_le_bytes());
                    self.buf.push(p.stick_x as u8);
                    self.buf.push(p.stick_y as u8);
                    runs += 1;
                    i += c;
                }
                self.buf[runs_at..runs_at + 2].copy_from_slice(&runs.to_le_bytes());
                self.bytes += self.buf.len() - start;
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn emit(line: &str) {
        #[wasm_bindgen::prelude::wasm_bindgen]
        extern "C" {
            #[wasm_bindgen(js_namespace = sm64miner, js_name = viz)]
            fn js_viz(line: &str);
        }
        js_viz(line);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn emit(_line: &str) {}

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen::prelude::wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = sm64miner, js_name = viz_state)]
        fn js_viz_state(key: &str, bytes: &[u8]);
        #[wasm_bindgen(js_namespace = sm64miner, js_name = viz_pert)]
        fn js_viz_pert(key: &str, json: &str);
        #[wasm_bindgen(js_namespace = sm64miner, js_name = viz_req)]
        fn js_viz_req(key: &str) -> Option<String>;
        #[wasm_bindgen(js_namespace = sm64miner, js_name = viz_path)]
        fn js_viz_path(key: &str, id: u32, flat: &[i32]);
        #[wasm_bindgen(js_namespace = sm64miner, js_name = viz_edges)]
        fn js_viz_edges(key: &str, bytes: &[u8]);
    }

    #[cfg(target_arch = "wasm32")]
    fn send_edges(key: &str, bytes: &[u8]) {
        js_viz_edges(key, bytes);
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn send_edges(_key: &str, _bytes: &[u8]) {}

    #[cfg(target_arch = "wasm32")]
    fn send_state(key: &str, bytes: &[u8]) {
        js_viz_state(key, bytes);
    }
    #[cfg(target_arch = "wasm32")]
    fn send_pert(key: &str, json: &str) {
        js_viz_pert(key, json);
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn send_pert(_key: &str, _json: &str) {}
    #[cfg(target_arch = "wasm32")]
    fn send_path(key: &str, id: u32, flat: &[i32]) {
        js_viz_path(key, id, flat);
    }
    /// The node ids the page asked this step's tree for (empty natively and without the page's board).
    #[cfg(target_arch = "wasm32")]
    fn requested(key: &str) -> Vec<u32> {
        js_viz_req(key).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn send_state(_key: &str, _bytes: &[u8]) {}
    #[cfg(not(target_arch = "wasm32"))]
    fn send_path(_key: &str, _id: u32, _flat: &[i32]) {}
    #[cfg(not(target_arch = "wasm32"))]
    fn requested(_key: &str) -> Vec<u32> {
        Vec::new()
    }

    /// Native: about once a second the frames this step has simulated so far go into `path` (a bare number), where the
    /// parent (`run_child`) reads them for the miner's frames/s counter ([`super::super::SIM_FRAMES`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn progress_file(path: String) -> Box<dyn FnMut(&Engine) + Send> {
        let mut last = Instant::now();
        Box::new(move |e: &Engine| {
            if last.elapsed().as_secs_f64() < 1.0 {
                return;
            }
            last = Instant::now();
            let _ = std::fs::write(&path, e.total_frames_emulated().to_string());
        })
    }

    /// Whether the page wants the feed now (`sm64miner.viz_on()`, a flag the page can flip).
    #[cfg(target_arch = "wasm32")]
    fn viz_on() -> bool {
        #[wasm_bindgen::prelude::wasm_bindgen]
        extern "C" {
            #[wasm_bindgen(js_namespace = sm64miner, js_name = viz_on)]
            fn js_viz_on() -> bool;
        }
        js_viz_on()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn viz_on() -> bool {
        true
    }
}

/// Tree sync between the browser build's workers (periodic, within one step). Every `sync_secs` the
/// search offers the best states of its frontier ([`Engine::sync_candidates`]: the top goals' best cells
/// by the engine's own ETA ranking, then frontier states by progress, spread over distinct places) as
/// records `{pads, level, pos, progress}`, the pads from the step's root. The exchange (`"tree_sync"`:
/// payload `{tag, recs}`, reply `{recs}` of the other workers' recent records for the same tag) is the
/// page's board. Each received record is replayed from the deepest node of this tree on its path and
/// added by [`Engine::sync_import`] as if this search's own expansion made it: checked against the
/// sender's position and progress, dropped when dead, wrong or when a cell has an earlier state.
/// A seed fixes the search given what it received.
pub mod tsync {
    use super::*;
    use crate::sys::Instant;
    use obrrt_search::engine::{Engine, SyncImport};

    const CELL_SEP: f32 = 150.0;
    /// Imports per sync round, and the largest message (JSON bytes) offered.
    const MAX_IMPORT: usize = 48;
    /// The new-zone fast path: seconds between its polls, states offered and states taken per poll.
    const FAST_SECS: f64 = 1.0;
    const FAST_K: usize = 2;
    const FAST_IMPORT: usize = 2;

    #[derive(Serialize, Deserialize)]
    struct Rec {
        /// Hex, 8 digits a pad (button, stick x, stick y).
        pads: String,
        level: u16,
        pos: [f32; 3],
        progress: (u8, i16),
        /// The first node its sender had in a new (level, area): offered at once, and taken by the others at once.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        zf: bool,
    }

    pub fn enc(p: &[GamePad]) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(p.len() * 8);
        for g in p {
            let _ = write!(s, "{:04x}{:02x}{:02x}", g.button, g.stick_x as u8, g.stick_y as u8);
        }
        s
    }

    pub fn dec(s: &str, max: usize) -> Option<Vec<GamePad>> {
        if s.len() % 8 != 0 || s.len() / 8 > max || !s.is_ascii() {
            return None;
        }
        (0..s.len() / 8)
            .map(|i| {
                let c = &s[i * 8..i * 8 + 8];
                Some(GamePad::new(u16::from_str_radix(&c[..4], 16).ok()?, u8::from_str_radix(&c[4..6], 16).ok()? as i8, u8::from_str_radix(&c[6..], 16).ok()? as i8))
            })
            .collect()
    }

    pub fn hook(job: &Job, ex: super::super::Exchange, root: &St, stats: Arc<std::sync::Mutex<SyncStats>>) -> Box<dyn FnMut(&mut Engine) + Send> {
        let (every, k, tag) = (job.sync_secs, job.sync_k.min(16), job.sync_tag.clone());
        // A path can't be longer than the step's frames budget allows.
        let max_pads = job.max_path as usize;
        let frame0 = root.frame;
        let mut last = Instant::now();
        let mut last_fast = Instant::now();
        Box::new(move |e: &mut Engine| {
            let due = last.elapsed().as_secs_f64() >= every;
            // Between rounds: about once a second, the first nodes in new zones go out and the others' are taken (at most
            // FAST_K sent and FAST_IMPORT taken per second a worker, so it stays cheap).
            let fast = !due && last_fast.elapsed().as_secs_f64() >= FAST_SECS;
            if !due && !fast {
                return;
            }
            last_fast = Instant::now();
            let ids = if due {
                last = Instant::now();
                e.sync_candidates(k, CELL_SEP)
            } else {
                Vec::new()
            };
            let firsts = e.sync_zone_firsts(FAST_K);
            let mk = |i: u32, zf: bool| {
                let n = &e.tree.nodes[i as usize];
                let pads = e.tree.path(i);
                (n.st.frame >= frame0 && pads.len() == (n.st.frame - frame0) as usize && pads.len() <= max_pads).then(|| Rec { pads: enc(&pads), level: n.st.level, pos: n.st.pos, progress: n.st.progress(), zf })
            };
            let recs: Vec<Rec> = firsts.iter().filter_map(|&i| mk(i, true)).chain(ids.iter().filter_map(|&i| mk(i, false))).collect();
            let msg = serde_json::json!({ "tag": tag, "recs": recs, "poll": fast }).to_string();
            let mut s = stats.lock().unwrap();
            if due {
                s.rounds += 1;
            }
            s.offered += recs.len() as u32;
            let Some(reply) = ex("tree_sync", &msg) else { return };
            #[derive(Deserialize)]
            struct Reply {
                recs: Vec<Rec>,
            }
            let Ok(r) = serde_json::from_str::<Reply>(&reply) else { return };
            let before = e.total_frames_emulated();
            let mut got = 0;
            for rec in r.recs.iter().take(if fast { FAST_IMPORT } else { MAX_IMPORT }) {
                let Some(pads) = dec(&rec.pads, max_pads) else { continue };
                s.received += 1;
                match e.sync_import(&pads, Some((rec.level, rec.pos, rec.progress))) {
                    Ok(SyncImport::Added(_)) => {
                        s.added += 1;
                        got += 1;
                    }
                    Ok(SyncImport::Dup) => s.dup += 1,
                    Ok(SyncImport::Known) => s.known += 1,
                    Ok(SyncImport::Bad) => s.bad += 1,
                    Err(_) => s.errors += 1,
                }
            }
            s.frames += e.total_frames_emulated() - before;
            if got > 0 || firsts.len() > 0 || (due && s.rounds <= 2) {
                println!("tree sync {}{}: offered {} ({} new-zone), received {}, added {} ({} frames replayed)", s.rounds, if fast { " (fast)" } else { "" }, recs.len(), firsts.len(), r.recs.len(), got, e.total_frames_emulated() - before);
            }
        })
    }
}

/// Hybrid mining in the browser build: the page's person plays from a node of this search's tree and the result
/// joins the tree; a position goal biases the objectives. Everything goes through the exchange (the page's board,
/// see web/src/lib/webminer/board.js `hybrid`), polled about every half second from the engine's round loop:
///
/// * `exchange("hybrid", {tag, vkey})` replies `{goal: {level, area, pos} | null, cmds: [...]}`: the position goal
///   (set on the engine each step) and the commands for this search that were not delivered before:
///   `{kind: "state", id, nid}` (the page wants node `nid`'s state: answered with `"hybrid_out"` `{kind: "state", ...}`:
///   the game state and perturbation of the node, its path from the step's root) and
///   `{kind: "insert", id, path, seg, weight}` (the played segment `seg`, hex pads, from the node reached by `path`, hex pads
///   from the step's root: added as a boosted manual node, [`Engine::insert_manual`]; answered with `{kind: "inserted", ...}`).
/// * `tag` is the step's sync tag (same step input and played prefix: the workers whose trees are the same step),
///   `vkey` the page's name of this search's step (`seed:frame0:level:area:goal`, as its viz feed names it).
pub mod hybrid {
    use super::*;
    use crate::sys::Instant;
    use obrrt_search::engine::{Engine, ManualInsert};

    #[derive(Deserialize)]
    struct GoalIn {
        level: u16,
        area: u8,
        pos: [f32; 3],
        /// The bubble's radius (game units; the engine clamps it).
        #[serde(default = "default_radius")]
        radius: f32,
        /// The bubble sampling proportion (the page's setting): the share of plans whose base state is drawn from inside it (0..1).
        #[serde(default = "default_share")]
        share: f64,
    }
    fn default_radius() -> f32 {
        300.0
    }
    fn default_share() -> f64 {
        0.35
    }

    #[derive(Deserialize)]
    #[serde(tag = "kind")]
    enum Cmd {
        #[serde(rename = "state")]
        State { id: u64, nid: u32 },
        #[serde(rename = "insert")]
        Insert { id: u64, path: String, seg: String, #[serde(default = "default_weight")] weight: f64 },
    }
    fn default_weight() -> f64 {
        8.0
    }

    #[derive(Deserialize, Default)]
    struct Reply {
        #[serde(default)]
        goal: Option<GoalIn>,
        #[serde(default)]
        cmds: Vec<Cmd>,
    }

    pub fn vkey(job: &Job, root: &St) -> String {
        format!("{}:{}:{}:{}:{:?}", job.seed, job.frame0, root.level, root.area, job.goal)
    }

    pub fn hook(job: &Job, ex: super::super::Exchange, root: &St) -> Box<dyn FnMut(&mut Engine) + Send> {
        let (tag, vkey, frame0, max_pads) = (job.sync_tag.clone(), vkey(job, root), root.frame, job.max_path as usize);
        let poll = serde_json::json!({ "tag": tag, "vkey": vkey }).to_string();
        let mut last = Instant::now();
        let mut first = true;
        let mut goal: Option<([u16; 2], [f32; 3], f32, f64)> = None;
                Box::new(move |e: &mut Engine| {
            if !first && last.elapsed().as_secs_f64() < 0.5 {
                return;
            }
            first = false;
            last = Instant::now();
            let Some(reply) = ex("hybrid", &poll) else { return };
            let Ok(r) = serde_json::from_str::<Reply>(&reply) else { return };
            let g = r.goal.map(|g| ([g.level, g.area as u16], g.pos, g.radius, g.share.clamp(0.0, 1.0)));
            if g != goal {
                goal = g;
                e.set_pos_goal(g.map(|(z, p, r, _)| ((z[0], z[1] as u8), p, r)));
                e.set_pos_goal_strength(0.0, g.map_or(0.0, |g| g.3));
                println!("sampling bubble: {g:?}");
            }
            for c in r.cmds {
                let out = match c {
                    Cmd::State { id, nid } => state(e, id, nid, &tag, &vkey, frame0),
                    Cmd::Insert { id, path, seg, weight } => insert(e, id, &path, &seg, weight, &tag, frame0, max_pads),
                };
                ex("hybrid_out", &out.to_string());
            }
        })
    }

    fn state(e: &mut Engine, id: u64, nid: u32, tag: &str, vkey: &str, frame0: u32) -> serde_json::Value {
        let res = (|| -> anyhow::Result<serde_json::Value> {
            let save = crate::hook::mining_save(&e.node_state(nid)?)?;
            let st = &e.tree.nodes[nid as usize].st;
            Ok(serde_json::json!({
                "kind": "state", "id": id, "ok": true, "tag": tag, "vkey": vkey, "nid": nid,
                "frame": st.frame - frame0, "level": st.level, "area": st.area, "pos": st.pos, "stars": st.stars,
                "path": tsync::enc(&e.node_path(nid)),
                "pert": serde_json::to_string(&save.pert)?,
                "state": hex::encode(save.game.into_bytes()),
            }))
        })();
        res.unwrap_or_else(|err| serde_json::json!({ "kind": "state", "id": id, "ok": false, "why": format!("{err:#}") }))
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(e: &mut Engine, id: u64, path: &str, seg: &str, weight: f64, tag: &str, frame0: u32, max_pads: usize) -> serde_json::Value {
        let (Some(mut pads), Some(seg)) = (tsync::dec(path, max_pads), tsync::dec(seg, max_pads)) else {
            return serde_json::json!({ "kind": "inserted", "id": id, "ok": false, "why": "unreadable pads" });
        };
        pads.extend(seg);
        if pads.len() > max_pads {
            return serde_json::json!({ "kind": "inserted", "id": id, "ok": false, "why": "past the longest possible run" });
        }
        match e.insert_manual(&pads, weight) {
            Ok(ManualInsert::Added(n)) => {
                let node = &e.tree.nodes[n as usize];
                let st = &node.st;
                println!("manual node {n}: {} frames from its parent {:?}, frame {}, level {} pos {:?}, boost {weight}", node.inputs.len(), node.parent, st.frame, st.level, st.pos);
                serde_json::json!({
                    "kind": "inserted", "id": id, "ok": true, "tag": tag, "nid": n, "parent": node.parent,
                    "frame": st.frame - frame0, "level": st.level, "area": st.area, "pos": st.pos, "stars": st.stars, "keys": st.keys,
                })
            }
            Ok(ManualInsert::Known) => serde_json::json!({ "kind": "inserted", "id": id, "ok": false, "why": "already in the tree" }),
            Ok(ManualInsert::Dead) => serde_json::json!({ "kind": "inserted", "id": id, "ok": false, "why": "the played segment ends in a dead state" }),
            Err(err) => serde_json::json!({ "kind": "inserted", "id": id, "ok": false, "why": format!("{err:#}") }),
        }
    }
}
