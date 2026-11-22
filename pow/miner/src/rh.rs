//! The automated miner's controller built on the user's rolling-horizon GA
//! (objective-rrt-v2 `RhController`, the ETA objectives), replacing the beam
//! search. Every worker game is an `SM64MiningGame` (the search crate's
//! `sm64coin` feature), so every rollout is perturbed exactly as the verifier
//! replays it. Deref trap guard: a shadow `SM64MiningGame` replays each
//! committed frame through `step` (the real perturbed step), the pads it
//! returns are the played pads sealed into the proof, and its position is
//! compared with the controller's at every commit (a controller that stepped
//! unperturbed would drift from the shadow at the first poke).
//!
//! Route: BoB's "Behind Chain Chomp's Gate" star the intended way, the legs of
//! objective-rrt-v2 tools/spec/routes/bob-chomp.json (bridge, castle door,
//! painting, ramp, elevator, Chomp's hill, post pounded 3 times, gate, star),
//! as a sliding window of spec objectives.
use anyhow::{Result, bail};
use obrrt_core::{GamePad, Objective, St, buttons::{A_BUTTON, START_BUTTON}, read_state};
use obrrt_search::ga::rhstep::{RhConfig, RhController};
use serde_json::Value;
use sm64pow::{Params, SM64MiningGame, Seed};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

const ROUTE: &str = include_str!("../routes/rh-bob-chomp.json");
/// Leg keys that only the route runner (legs) reads.
const LEG_ONLY: &[&str] = &["doc", "dialogs", "reference", "budget", "stable_frames", "expect_frames", "require_next", "done_requires_next", "moves", "window_break"];

/// The route's legs as objectives, in order.
pub fn route_objectives() -> Result<Vec<Arc<dyn Objective>>> {
    let doc: Value = serde_json::from_str(ROUTE)?;
    let mut out: Vec<Arc<dyn Objective>> = Vec::new();
    for (i, leg) in doc["legs"].as_array().ok_or_else(|| anyhow::anyhow!("route: no legs"))?.iter().enumerate() {
        let mut m = leg.as_object().unwrap().clone();
        for k in LEG_ONLY {
            m.remove(*k);
        }
        let mut specs = obrrt_spec::Spec::parse(&Value::Object(m), &format!("$.legs[{i}]"), None)?;
        out.push(Arc::new(specs.pop().ok_or_else(|| anyhow::anyhow!("leg {i}: no spec"))?));
    }
    Ok(out)
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
}

impl Default for RhOptions {
    fn default() -> Self {
        RhOptions { pop: 32, horizon: 600, commit: 4, gens: 1, threads: 4, window: 3, seed: 1, stall: 900, max_retries: 40 }
    }
}

pub use crate::tracker::Mined;

/// obrrt2's fixed boot: A on alternate frames, Start at 150-160 and 200-300,
/// 1680 frames; then idle until Mario is controllable and has stood still for
/// 10 frames. Played through the perturbation like everything else.
fn boot(game: &mut SM64MiningGame, played: &mut Vec<GamePad>, max_frames: usize) -> Result<St> {
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
    let mut still = 0;
    while still < 10 || !st.controllable() {
        let before = st.pos;
        played.push(game.step(GamePad::default())?.0);
        st = read_state(game, played.len() as u32, false);
        still = if st.pos == before { still + 1 } else { 0 };
        if played.len() >= max_frames {
            bail!("boot: Mario didn't stand still");
        }
    }
    Ok(read_state(game, played.len() as u32, true))
}

pub fn mine(seed: &Seed, params: &Params, o: &RhOptions, stop: &AtomicBool, mut log: impl FnMut(String)) -> Result<Option<Mined>> {
    let t0 = Instant::now();
    let objs = route_objectives()?;
    let max = params.max_frames as usize;
    let mut game = sm64pow::new_mining_game(seed, params)?;
    let mut played: Vec<GamePad> = Vec::new();
    let st = boot(&mut game, &mut played, max)?;
    log(format!("booted at frame {}", st.frame));
    let cfg = RhConfig { pop: o.pop, h: o.horizon, k: o.commit, g: o.gens, seed: o.seed, ..Default::default() };
    // The workers are forks of `game`: mining games, perturbation included.
    let mut ctl = RhController::new(&game, o.threads, &game.save_state(), st.clone(), cfg.clone())?;
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
            return Ok(None);
        }
        if played.len() >= max {
            log("out of frames".into());
            return Ok(None);
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
                return Ok(None);
            }
            log(format!("{} at frame {} on {}: rewind to frame {} (retry {retries})", if out.stuck { "stuck" } else { "stalled" }, ctl.state().frame, objs[next].key(), ckpt.2));
            shadow.load_state(&ckpt.0)?;
            played.truncate(ckpt.2);
            ctl.load(&ckpt.0, ckpt.1.clone(), true)?;
            ctl.set_config(RhConfig { salt: retries, ..cfg.clone() });
            continue;
        }
        for p in &out.pads {
            played.push(shadow.step(*p)?.0);
            let gs = shadow.state()?;
            if rules.is_success(&shadow, &gs) {
                let seconds = t0.elapsed().as_secs_f64();
                return Ok(Some(Mined { pads: played, seconds, sim_frames: ctl.frames }));
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
                return Ok(None);
            }
        }
        if last_log.elapsed().as_secs() >= 30 {
            last_log = Instant::now();
            log(format!("frame {} obj {}/{} eta {:.0} sim {} pos {:?} act {:#x}", played.len(), next, objs.len(), out.eta, ctl.frames, ctl.state().pos, ctl.state().action));
        }
    }
}
