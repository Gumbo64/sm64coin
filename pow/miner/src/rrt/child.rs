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
    g.load_state(&SaveState::from_bytes(crate::sys::fs::read(&job.state_file)?)?)?;
    let game = SM64MiningGame { game: g, pert: job.pert.clone() };
    check_layout(&game)?;
    let root = read_state(&game, job.frame0, true);
    println!("root: frame {} level {} course {} area {} act_select {} act_init {} cursor {} action {:#x} play_mode {} pos {:?}; goal {:?} target {:?} need {:#x}", root.frame, root.level, root.course, root.area, root.act_select, root.act_init, root.act_cursor, root.action, root.play_mode, root.pos, job.goal, job.target, job.need_flags);
    obrrt_search::envconfig::after_root(&root);

    let generator = guide::wrap(obrrt_spec::wrap(Arc::new(Registry))?)?;
    let save = game.save_state();
    let mut engine = Engine::new_hooked(cfg, &crate::hook::hooked(&game)?, job.threads, Vec::new(), crate::hook::hooked_save(&save), root.clone(), generator)?;
    if job.viz {
        engine.on_round = Some(viz::sampler(job, &root));
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
///   // up to NODES new nodes since the last sample (evenly thinned; flags bit 0: more progress than the root)
///   "pads": [button, stickX, stickY, ...]   // only when `best` changed: its inputs from the step's start
/// }
/// ```
pub mod viz {
    use super::*;
    use crate::sys::Instant;
    use obrrt_search::engine::Engine;

    const NODES: usize = 500;

    pub fn sampler(job: &Job, root: &St) -> Box<dyn FnMut(&Engine) + Send> {
        let head = serde_json::json!({ "frame0": job.frame0, "seed": job.seed, "goal": format!("{:?}", job.goal), "level": root.level, "area": root.area, "pos": root.pos });
        let (frame0, root_prog) = (job.frame0, root.progress());
        let mut last = Instant::now();
        let t0 = Instant::now();
        let mut seen = 1usize; // nodes below this index were sampled (the root isn't a sample)
        let mut best: Option<usize> = None;
        Box::new(move |e: &Engine| {
            // The page can switch the feed off while the search runs: then this is the whole cost.
            if last.elapsed().as_secs_f64() < 1.0 {
                return;
            }
            // Feed off: only a tiny progress line (frames simulated so far in this step), about once a second,
            // for the page's frames/s readout.
            if !viz_on() {
                last = Instant::now();
                emit(&format!("{{\"progress\":1,\"sim_frames\":{}}}", e.total_frames_emulated()));
                return;
            }
            last = Instant::now();
            let nodes = &e.tree.nodes;
            let n = nodes.len();
            let key = |i: usize| (nodes[i].st.progress(), std::cmp::Reverse(nodes[i].st.frame));
            let mut changed = false;
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
            seen = n;
            let mut msg = serde_json::json!({ "step": head, "t": t0.elapsed().as_secs_f64(), "nodes_total": n, "sim_frames": e.total_frames_emulated(), "nodes": sample });
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

    #[derive(Serialize, Deserialize)]
    struct Rec {
        /// Hex, 8 digits a pad (button, stick x, stick y).
        pads: String,
        level: u16,
        pos: [f32; 3],
        progress: (u8, i16),
    }

    fn enc(p: &[GamePad]) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(p.len() * 8);
        for g in p {
            let _ = write!(s, "{:04x}{:02x}{:02x}", g.button, g.stick_x as u8, g.stick_y as u8);
        }
        s
    }

    fn dec(s: &str, max: usize) -> Option<Vec<GamePad>> {
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
        Box::new(move |e: &mut Engine| {
            if last.elapsed().as_secs_f64() < every {
                return;
            }
            last = Instant::now();
            let ids = e.sync_candidates(k, CELL_SEP);
            let recs: Vec<Rec> = ids
                .iter()
                .filter_map(|&i| {
                    let n = &e.tree.nodes[i as usize];
                    let pads = e.tree.path(i);
                    (n.st.frame >= frame0 && pads.len() == (n.st.frame - frame0) as usize && pads.len() <= max_pads).then(|| Rec { pads: enc(&pads), level: n.st.level, pos: n.st.pos, progress: n.st.progress() })
                })
                .collect();
            let msg = serde_json::json!({ "tag": tag, "recs": recs }).to_string();
            let mut s = stats.lock().unwrap();
            s.rounds += 1;
            s.offered += recs.len() as u32;
            let Some(reply) = ex("tree_sync", &msg) else { return };
            #[derive(Deserialize)]
            struct Reply {
                recs: Vec<Rec>,
            }
            let Ok(r) = serde_json::from_str::<Reply>(&reply) else { return };
            let before = e.total_frames_emulated();
            let mut got = 0;
            for rec in r.recs.iter().take(MAX_IMPORT) {
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
            if got > 0 || s.rounds <= 2 {
                println!("tree sync {}: offered {}, received {}, added {} ({} frames replayed)", s.rounds, recs.len(), r.recs.len(), got, e.total_frames_emulated() - before);
            }
        })
    }
}
