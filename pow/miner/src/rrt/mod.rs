//! The RRT miner: the user's objective-RRT tree search (objective-rrt-v2) with
//! its objectives, driving the AutoMiner to the game's next progress point
//! from any state of the cycle.
//!
//! A block starts from power-on or the parent's end state. The tree runs as
//! the seg120 campaign does, per zone, but all inside one block:
//!
//! * castle (grounds/castle/courtyard): a zone-locked tree whose only good exit
//!   is the target course (`plan::targets`: the course with the most stars
//!   missing the star counts allow); a castle star on the way also ends it;
//! * course: a zone-locked tree that stops at the first state with more stars
//!   or keys than the block started with.
//!
//! Every tree step is a child process (`rrtrun`, child.rs) over perturbed
//! games. This process owns the *shadow*: one `SM64MiningGame` that plays the
//! block for real, frame by frame, through `step` (the perturbed one, as the
//! verifier replays), records the played pads, ends the block on the first
//! frame the rules call success, and compares its position with the tree's at
//! every step boundary. A tree that had stepped a game unperturbed would drift
//! from the shadow at the first poke and the block is refused. The caller
//! still verifies the sealed proof with `sm64pow::chain`.
pub mod child;
pub mod envcfg;
pub mod plan;

use crate::rh::{Attempt, Mined, boot, settle};
use anyhow::{Result, bail};
use child::{Job, StepOut, is_hub_level};
use obrrt_core::{GamePad, read_state};
use sm64pow::proof::Start;
use sm64pow::sm64_binds::SaveState;
use sm64pow::{Params, SM64MiningGame, Seed};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct RrtOptions {
    pub threads: usize,
    /// Emulated frames (all threads) a castle tree / a course tree may use.
    pub hub_frames: u64,
    pub course_frames: u64,
    /// Wall-clock cap of one tree.
    pub step_seconds: f64,
    /// Courses tried from the castle in one block.
    pub max_targets: usize,
    pub mem_mb: usize,
    pub seed: u64,
    /// Frames a run may take (0: the most any difficulty allows).
    pub frame_cap: usize,
    /// Scratch directory for the step files.
    pub scratch: PathBuf,
    /// The `rrtrun` binary (default: next to the running one).
    pub child: Option<PathBuf>,
    /// Keep the step logs.
    pub keep: bool,
    /// The ROM the child loads (default: sm64pow's search path).
    pub rom: Option<String>,
}

impl Default for RrtOptions {
    fn default() -> Self {
        RrtOptions { threads: 4, hub_frames: 16_000_000, course_frames: 60_000_000, step_seconds: 900.0, max_targets: 3, mem_mb: 1500, seed: 1, frame_cap: 0, scratch: std::env::temp_dir().join("sm64coin-rrt"), child: None, keep: false, rom: None }
    }
}

pub fn coverage() -> Vec<(&'static str, &'static str)> {
    vec![
        ("star: any main course (BoB-RR) the castle tree reaches and the course tree solves", "objective-RRT per zone (castle -> course), most stars missing first"),
        ("star: castle stars (Toad, MIPS) on the way", "castle tree stops on any progress"),
    ]
}

fn child_exe(o: &RrtOptions) -> Result<PathBuf> {
    if let Some(c) = &o.child {
        return Ok(c.clone());
    }
    let me = std::env::current_exe()?;
    let p = me.with_file_name("rrtrun");
    if !p.exists() {
        bail!("{} not found (cargo build --release -p sm64coin-miner --bin rrtrun)", p.display());
    }
    Ok(p)
}

/// Runs the tree step `job` from `game` in a child process.
fn run_child(o: &RrtOptions, game: &SM64MiningGame, mut job: Job, tag: &str, stop: &AtomicBool) -> Result<StepOut> {
    std::fs::create_dir_all(&o.scratch)?;
    let base = o.scratch.join(format!("{}-{tag}", std::process::id()));
    let state = format!("{}.state", base.display());
    std::fs::write(&state, game.game.save_state().as_bytes())?;
    job.state_file = state.clone();
    job.pert = game.pert.clone();
    job.out_dir = format!("{}.run", base.display());
    let (jf, of, lf) = (format!("{}.job.json", base.display()), format!("{}.out.json", base.display()), format!("{}.log", base.display()));
    std::fs::write(&jf, serde_json::to_string(&job)?)?;
    let mut child = std::process::Command::new(child_exe(o)?)
        .arg(&jf)
        .arg(&of)
        .stdout(std::fs::File::create(&lf)?)
        .stderr(std::fs::File::options().append(true).open(&lf)?)
        .spawn()?;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&state);
            bail!("stopped");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = std::fs::remove_file(&state);
    let _ = std::fs::remove_dir_all(&job.out_dir);
    if !status.success() {
        let tail: String = std::fs::read_to_string(&lf).unwrap_or_default().lines().rev().take(5).collect::<Vec<_>>().join(" | ");
        bail!("rrtrun {tag} failed ({status}): {tail}");
    }
    let out: StepOut = serde_json::from_str(&std::fs::read_to_string(&of)?)?;
    let _ = std::fs::remove_file(&of);
    let _ = std::fs::remove_file(&jf);
    if !o.keep {
        let _ = std::fs::remove_file(&lf);
    }
    Ok(out)
}

/// Plays the step's pads on the shadow, stopping at the first success.
/// Ok(true): the block's progress point was made (the shadow's `played` ends there).
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

fn guard(shadow: &SM64MiningGame, played: &[GamePad], out: &StepOut) -> Result<()> {
    let ss = read_state(shadow, played.len() as u32, false);
    if ss.pos != out.pos || ss.level != out.level || ss.frame != out.frame {
        bail!(
            "the tree's winner and the perturbed replay diverge at frame {} (pos {:?} level {} vs tree pos {:?} level {} frame {}): a game was stepped unperturbed",
            played.len(), ss.pos, ss.level, out.pos, out.level, out.frame
        );
    }
    Ok(())
}

/// Mines one block from `start` to the next progress point.
pub fn mine(start: Start, parent: Option<&SaveState>, seed: &Seed, params: &Params, o: &RrtOptions, stop: &AtomicBool, mut log: impl FnMut(String)) -> Result<Attempt> {
    let t0 = Instant::now();
    let max = if o.frame_cap > 0 { o.frame_cap } else { sm64pow::rules().max_frames_any() as usize };
    let (mut game, before) = sm64pow::chain::mining_start(start, parent, seed, params)?;
    let mut played: Vec<GamePad> = Vec::new();
    let st = match start {
        Start::PowerOn => boot(&mut game, &mut played, max)?,
        Start::Parent => settle(&mut game, &mut played, max)?,
    };
    log(format!("start {:?} at {} stars {} keys, ready at frame {} (level {} {})", start, before.stars, before.keys, st.frame, st.level, plan::name(st.level)));
    let mut sim = 0u64;
    let mut banned: Vec<u16> = Vec::new();
    let mut tries = 0usize;
    let mut salt = o.seed;
    let job = |target: Option<u16>, skip: Vec<u16>, budget: u64, played: usize, salt: u64| Job {
        rom: String::new(),
        state_file: String::new(),
        pert: game_pert_placeholder(),
        frame0: played as u32,
        target,
        skip,
        budget,
        seconds: o.step_seconds,
        threads: o.threads,
        seed: salt,
        mem_mb: o.mem_mb,
        max_path: (max - played) as u32,
        out_dir: String::new(),
    };
    let rom = match &o.rom {
        Some(r) => std::fs::canonicalize(r)?.to_string_lossy().into_owned(),
        None => sm64pow::default_rom_path().ok_or_else(|| anyhow::anyhow!("no ROM (RrtOptions::rom)"))?,
    };
    let mine_result = loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(Attempt::Gave("stopped".into()));
        }
        let st = read_state(&game, played.len() as u32, false);
        let finish = |played: Vec<GamePad>, sim: u64| Attempt::Found(Mined { pads: played, seconds: t0.elapsed().as_secs_f64(), sim_frames: sim });
        if is_hub_level(st.level) {
            let cands = plan::targets(&st, &banned);
            if cands.is_empty() || tries >= o.max_targets {
                break Attempt::Gave(format!("no course left to try from the castle (tried {banned:?})"));
            }
            tries += 1;
            salt += 1;
            let target = cands[0];
            let ckpt = (game.save_state(), played.len());
            log(format!("castle at frame {}: {} stars; target {} ({} stars had there); budget {} frames", played.len(), st.stars, plan::name(target), plan::got(&st, plan::COURSES.iter().find(|c| c.0 == target).unwrap().1), o.hub_frames));
            let mut j = job(Some(target), plan::skip_all_but(Some(target)), o.hub_frames, played.len(), salt);
            j.rom = rom.clone();
            let out = match run_child(o, &game, j, &format!("hub{tries}"), stop) {
                Ok(x) => x,
                Err(e) if stop.load(Ordering::Relaxed) => return Ok(Attempt::Gave(format!("stopped ({e})"))),
                Err(e) => return Err(e),
            };
            sim += out.emulated;
            if !out.found {
                log(format!("  castle tree: {} ({:.0} s)", out.note, t0.elapsed().as_secs_f64()));
                banned.push(target);
                continue;
            }
            log(format!("  castle tree: exit to level {} ({}) at frame {} (+{}), {} frames emulated, {} nodes ({:.0} s)", out.level, plan::name(out.level), out.frame, out.pads.len(), out.emulated, out.nodes, t0.elapsed().as_secs_f64()));
            if replay(&mut game, &mut played, &child::pads_of(&out.pads), &before)? {
                break finish(played, sim);
            }
            guard(&game, &played, &out)?;
            if is_hub_level(out.level) {
                // A castle star on the way that the shadow did not see as success: inconsistent.
                bail!("the castle tree's winner is in the castle without progress");
            }
            if idle_until_controllable(&mut game, &mut played, max, &before)? {
                break finish(played, sim);
            }
            // The course tree.
            let st = read_state(&game, played.len() as u32, false);
            salt += 1;
            let mut j = job(None, Vec::new(), o.course_frames, played.len(), salt);
            j.rom = rom.clone();
            log(format!("  in {} at frame {} (act select {} controllable {} modal {} action {:#x} act_init {} cursor {}); course tree, budget {} frames", plan::name(st.level), played.len(), st.act_select, st.controllable(), st.modal(), st.action, st.act_init, st.act_cursor, o.course_frames));
            let out = match run_child(o, &game, j, &format!("course{tries}"), stop) {
                Ok(x) => x,
                Err(e) if stop.load(Ordering::Relaxed) => return Ok(Attempt::Gave(format!("stopped ({e})"))),
                Err(e) => return Err(e),
            };
            sim += out.emulated;
            if out.found {
                log(format!("  course tree: progress at frame {} (+{}), {} frames emulated, {} nodes ({:.0} s)", out.frame, out.pads.len(), out.emulated, out.nodes, t0.elapsed().as_secs_f64()));
                if replay(&mut game, &mut played, &child::pads_of(&out.pads), &before)? {
                    break finish(played, sim);
                }
                bail!("the course tree's winner made progress but the perturbed replay did not (frame {}): a game was stepped unperturbed", played.len());
            }
            log(format!("  course tree: {} ({:.0} s); back to the castle, banning {}", out.note, t0.elapsed().as_secs_f64(), plan::name(target)));
            banned.push(target);
            game.load_state(&ckpt.0)?;
            played.truncate(ckpt.1);
        } else {
            // Inside a course already (the parent's end state): just the course tree.
            if tries >= 1 {
                break Attempt::Gave("the course tree found no progress".into());
            }
            tries += 1;
            salt += 1;
            let mut j = job(None, Vec::new(), o.course_frames, played.len(), salt);
            j.rom = rom.clone();
            log(format!("in {} at frame {}; course tree, budget {} frames", plan::name(st.level), played.len(), o.course_frames));
            let out = run_child(o, &game, j, "course0", stop)?;
            sim += out.emulated;
            if !out.found {
                log(format!("  course tree: {}", out.note));
                break Attempt::Gave(out.note);
            }
            if replay(&mut game, &mut played, &child::pads_of(&out.pads), &before)? {
                break finish(played, sim);
            }
            bail!("the course tree's winner made progress but the perturbed replay did not: a game was stepped unperturbed");
        }
    };
    Ok(mine_result)
}

fn game_pert_placeholder() -> sm64pow::game::Perturber {
    sm64pow::game::Perturber::new(&[0; 32], &sm64pow::game::poke_config(&sm64pow::params(1).rng))
}
