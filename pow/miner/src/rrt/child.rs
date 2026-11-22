//! One tree step of the RRT miner, run in its own process (`rrtrun`): the
//! user's objective-RRT engine (objective-rrt-v2, feature `sm64coin`) from a
//! start state, over perturbed games only.
//!
//! Why a process: the engine and the objectives read many `OBRRT_*` switches
//! once per process (the campaign's settings, the exit-skip list, the stop
//! rule), so each step of a block (the castle tree, then the course tree) gets
//! a fresh process, as the seg120 campaign runs one `obrrt2` per step.
//!
//! The start is a mining save: the game's memory plus the block's
//! perturbation state (the engine stores the perturbation in every node, see
//! the search crate's `store::Delta`). Every worker game is a fork of the
//! mining game, so every rollout, copy, sync, save and load is the perturbed
//! one. The step returns the wanted pads of the earliest state that made
//! progress (or entered the target course); the caller replays them through
//! its own shadow `SM64MiningGame`, which is what the verifier replays.
use anyhow::{Result, bail};
use obrrt_core::{check_layout, read_state};
use obrrt_objectives::Registry;
use obrrt_search::Engine;
use serde::{Deserialize, Serialize};
use sm64pow::game::Perturber;
use sm64pow::sm64_binds::{GamePad, SaveState};
use sm64pow::SM64MiningGame;
use std::sync::Arc;

/// Courses by level number (campaign.py's COURSE).
pub const HUB: [u16; 3] = [6, 16, 26];

#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub rom: String,
    /// The start's game memory (raw `SaveState` bytes).
    pub state_file: String,
    pub pert: Perturber,
    /// Frames played since the block's start (the root's frame number).
    pub frame0: u32,
    /// Castle trees: the course to enter (every other course is skipped).
    pub target: Option<u16>,
    /// Levels the tree is told not to take (campaign's EXIT_SKIP).
    pub skip: Vec<u16>,
    /// Emulated-frame budget of the tree.
    pub budget: u64,
    /// Wall-clock cap in seconds.
    pub seconds: f64,
    pub threads: usize,
    pub seed: u64,
    pub mem_mb: usize,
    /// The path to the winner may not be longer than this many frames.
    pub max_path: u32,
    pub out_dir: String,
}

#[derive(Default, Serialize, Deserialize)]
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
    pub exits: usize,
    pub note: String,
}

/// The campaign's tree settings (campaign.py `ENV`), by environment (read by
/// the objectives and the GA) and by the engine's own switches.
const ENV: &[(&str, &str)] = &[
    ("OBRRT_SOFT_DEDUP", "1"), ("OBRRT_ROUND", "64"), ("OBRRT_BOOT_NOWAIT", "1"), ("OBRRT_TECH", "all"), ("OBRRT_NEW_ELIGIBLE", "1"),
    ("OBRRT_STARSET_KEY", "1"), ("OBRRT_PICK_BY_B", "0"), ("OBRRT_SWIM_HOLD", "1"), ("OBRRT_GP_CATCH", "1"), ("OBRRT_KEEP_WIP", "1"),
    ("OBRRT_MOVE_WEIGHT", "0.75"), ("OBRRT_CHAIN", "route"), ("OBRRT_SAMPLER", "rate"), ("OBRRT_MOVES", "1"), ("OBRRT_MOVE_GA", "1"),
    ("OBRRT_MOVE_GA_EXTRA", "1"), ("OBRRT_CALM", "1"), ("OBRRT_LAKITU_ARC", "1"), ("OBRRT_PAINTING_FAST", "1"), ("OBRRT_ADD", "last_gen"),
    ("OBRRT_KINSHIP_MULT", "10"), ("OBRRT_ZONE_LOCK", "1"),
];

pub fn is_hub_level(level: u16) -> bool {
    HUB.contains(&level)
}

/// Runs one tree step in this process. Call once per process.
pub fn run_step(job: &Job) -> Result<StepOut> {
    use std::sync::atomic::Ordering::Relaxed;
    // Settings first: the objectives read them lazily, once.
    // SAFETY: single-threaded here (nothing else has started).
    unsafe {
        for (k, v) in ENV {
            std::env::set_var(k, v);
        }
        std::env::set_var("RAYON_NUM_THREADS", job.threads.to_string());
        std::env::set_var("OBRRT_STOP_EXIT_GRACE", "0");
        std::env::set_var("OBRRT_STOP_PROGRESS", "1");
        if job.skip.is_empty() {
            std::env::remove_var("OBRRT_EXIT_SKIP");
        } else {
            std::env::set_var("OBRRT_EXIT_SKIP", job.skip.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","));
        }
    }
    let (tech, names) = obrrt_objectives::setup::from_env().map_err(anyhow::Error::msg)?;
    let _ = (tech, names);
    rayon::ThreadPoolBuilder::new().num_threads(job.threads).build_global().ok();

    sm64pow::init(std::fs::read(&job.rom)?)?;
    let mut g = sm64pow::generator()?.create_game()?;
    g.load_state(&SaveState::from_bytes(std::fs::read(&job.state_file)?)?)?;
    let game = SM64MiningGame { game: g, pert: job.pert.clone() };
    check_layout(&game)?;
    let root = read_state(&game, job.frame0, true);
    println!("root: frame {} level {} course {} area {} act_select {} act_init {} cursor {} action {:#x} play_mode {} pos {:?}", root.frame, root.level, root.course, root.area, root.act_select, root.act_init, root.act_cursor, root.action, root.play_mode, root.pos);
    let hub = is_hub_level(root.level);
    if hub {
        obrrt_core::geo::set_enabled(true);
    }
    obrrt_search::tree::ZONE_LOCK.store(root.level as i32, Relaxed);
    obrrt_search::tree::STAR_WINDOW.store(99, Relaxed);

    let mut cfg = obrrt_search::Config::default();
    super::envcfg::apply(&mut cfg, job.mem_mb)?;
    cfg.out_dir = job.out_dir.clone();
    cfg.seed = job.seed;
    cfg.minutes = job.seconds / 60.0;
    cfg.max_frames = Some(job.budget);
    std::fs::create_dir_all(&job.out_dir)?;

    let generator = obrrt_spec::wrap(Arc::new(Registry))?;
    let save = game.save_state();
    let mut engine = Engine::new_mining(cfg, &game, job.threads, Vec::new(), save, root.clone(), generator)?;
    engine.run()?;

    // The earliest state that made progress, or (castle) entered the target.
    let tree = &engine.tree;
    let p0 = root.progress();
    let mut best: Option<usize> = None;
    let mut exits = 0;
    for (i, n) in tree.nodes.iter().enumerate().skip(1) {
        let st = &n.st;
        let progress = st.progress() > p0;
        // An exit is a state Mario can act from (the star select screen stays inside).
        let entered = hub && job.target.is_some_and(|t| st.level == t) && st.controllable() && !st.modal();
        if !(progress || entered) {
            continue;
        }
        exits += 1;
        if st.frame.saturating_sub(job.frame0) > job.max_path {
            continue;
        }
        if best.is_none_or(|b| st.frame < tree.nodes[b].st.frame) {
            best = Some(i);
        }
    }
    let mut out = StepOut { emulated: engine.total_frames_emulated(), nodes: tree.nodes.len(), exits, ..Default::default() };
    let Some(b) = best else {
        out.note = format!("no state with progress or the target in {} frames, {} nodes", out.emulated, out.nodes);
        return Ok(out);
    };
    let mut chain = Vec::new();
    let mut at = Some(b as u32);
    while let Some(i) = at {
        chain.push(i);
        at = tree.nodes[i as usize].parent;
    }
    for &i in chain.iter().rev() {
        for p in &tree.nodes[i as usize].inputs {
            out.pads.push((p.button, p.stick_x, p.stick_y));
        }
    }
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
    Ok(out)
}

pub fn pads_of(v: &[(u16, i8, i8)]) -> Vec<GamePad> {
    v.iter().map(|&(b, x, y)| GamePad::new(b, x, y)).collect()
}
