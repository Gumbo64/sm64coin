//! The automated miner's controller: follow a reference route through the
//! perturbation.
//!
//! A reference route (an m64 that reaches the success condition when played
//! unperturbed) gives the target state for every frame. Under the
//! perturbation, replaying it fails within seconds, so the tracker re-plans
//! continuously, rolling-horizon style: from the committed state it simulates
//! many candidate continuations (the reference inputs, time-shifted and
//! mutated), each through the real perturbation (`sm64pow::mine_step`), scores
//! how far along the reference each gets and how closely, and commits the
//! first frames of the best. Because candidates run through the exact
//! perturbation the verifier applies, whatever is committed verifies.
//!
//! When progress stalls it backtracks to an earlier checkpoint and tries
//! again with fresh randomness.

use anyhow::Result;
use obrrt_core::{St, read_state};
use rand::{Rng, SeedableRng, rngs::SmallRng};
use rayon::prelude::*;
use sm64_binds::GamePad;
use sm64pow::{SM64MiningGame, SM64MiningSave};
use sm64pow::{Params, Seed};
use std::sync::atomic::{AtomicBool, Ordering};
use crate::sys::Instant;

/// What phase of the game a state is in; a change of phase is a landmark.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Phase {
    pub(crate) level: u16,
    pub(crate) area: u8,
    play_mode: i16,
    dialog: i16,
    menu_mode: i16,
    act_select: bool,
    transition: bool,
}

fn phase(s: &St) -> Phase {
    Phase {
        level: s.level,
        area: s.area,
        play_mode: s.play_mode,
        dialog: s.dialog,
        menu_mode: s.menu_mode,
        act_select: s.act_select,
        transition: s.transition != 0,
    }
}

/// The compact per-frame state the tracker compares.
#[derive(Clone, Copy, Debug)]
pub struct Snap {
    pub(crate) phase: Phase,
    /// Frames since the phase last changed.
    age: u32,
    pub(crate) pos: [f32; 3],
    vel: [f32; 3],
    fvel: f32,
    yaw: i16,
    action: u32,
    pub(crate) holding: bool,
    stars: i16,
    lives: i8,
    /// Mario's position changed since the previous frame.
    pub(crate) moved: bool,
    cam_yaw: i16,
    /// Frames since Mario picked up what he holds (fuses run from then).
    held_age: u32,
    /// Frames Mario's position hasn't changed.
    still: u32,
}

impl Snap {
    pub(crate) fn new(s: &St, prev: Option<&Snap>) -> Snap {
        let ph = phase(s);
        let age = match prev {
            Some(p) if p.phase == ph => p.age + 1,
            _ => 0,
        };
        Snap {
            phase: ph,
            age,
            pos: s.pos,
            vel: s.vel,
            fvel: s.forward_vel,
            yaw: s.yaw,
            action: s.action,
            holding: s.holding,
            stars: s.stars,
            lives: s.lives,
            moved: prev.is_none_or(|p| p.pos != s.pos),
            still: match prev {
                Some(p) if p.pos == s.pos => p.still + 1,
                _ => 0,
            },
            cam_yaw: s.cam_yaw,
            held_age: match prev {
                Some(p) if s.holding && p.holding => p.held_age + 1,
                _ => 0,
            },
        }
    }
}

/// How far apart two states are, in rough "frames of play". `None` when
/// they're in different levels or areas.
fn dist(a: &Snap, r: &Snap, r_waiting: bool) -> Option<f32> {
    dist_scaled(a, r, r_waiting, 25.0, 0.25)
}

/// `pos_scale`: world units per unit of distance (smaller = stricter).
fn dist_scaled(a: &Snap, r: &Snap, r_waiting: bool, pos_scale: f32, vel_w: f32) -> Option<f32> {
    if a.phase.level != r.phase.level || a.phase.area != r.phase.area {
        return None;
    }
    let dp = ((a.pos[0] - r.pos[0]).powi(2) + (a.pos[1] - r.pos[1]).powi(2) + (a.pos[2] - r.pos[2]).powi(2)).sqrt();
    let dv = ((a.vel[0] - r.vel[0]).powi(2) + (a.vel[1] - r.vel[1]).powi(2) + (a.vel[2] - r.vel[2]).powi(2)).sqrt()
        + (a.fvel - r.fvel).abs();
    let dyaw = (a.yaw.wrapping_sub(r.yaw) as f32).abs() / 4096.0;
    // Mostly where Mario is: the route can be followed at another speed or
    // with other moves after a poke breaks a jump chain.
    let mut d = dp / pos_scale + vel_w * (dv / 12.0 + dyaw);
    if a.action != r.action {
        d += 1.0;
    }
    if a.holding != r.holding {
        d += 30.0;
    } else if a.holding {
        // Carrying something with a fuse (a Bob-omb): keep the reference's
        // schedule from the pickup on, or it goes off in the wrong place.
        d += (a.held_age as f32 - r.held_age as f32).abs() * 0.5;
    }
    if a.phase != r.phase {
        d += 25.0;
    }
    // Time-in-phase only matters where the reference Mario stands still
    // (menus, cutscenes, dialogs), to tell those frames apart; there it must
    // weigh more than 1 so it beats "further along". While moving, position
    // already says where we are, and a dialog closed early must not count.
    // (Judged by position: in cutscenes the velocity can read non-zero while
    // Mario is frozen in place.)
    if r_waiting {
        // Either since the phase began or since Mario stopped: a Mario who
        // got to a standstill at another time is still at the same place in
        // the wait (e.g. stuck in the ground, mashing).
        d += (a.age as f32 - r.age as f32).abs().min((a.still as f32 - r.still as f32).abs()) * 1.5;
    }
    if a.lives < r.lives || a.stars < r.stars {
        d += 200.0;
    }
    Some(d)
}

/// How many reference frames of progress one unit of distance is worth.
const CLOSENESS: f32 = 3.0;
/// The committed state only counts as being at a reference frame when
/// closer than this.
pub(crate) const ON_TRACK: f32 = 12.0;
/// How far back to look for the reference frame we're at.
pub(crate) const LOOK_BACK: usize = 120;
/// How far ahead (beyond the horizon) to look for it.
pub(crate) const LOOK_AHEAD: usize = 150;

pub struct Reference {
    pub inputs: Vec<GamePad>,
    /// `snaps[i]`: the state after `i + 1` frames.
    pub snaps: Vec<Snap>,
    /// Whether `snaps[i]` is inside a long stretch where Mario doesn't move
    /// (menus, cutscenes, dialogs): there time is the only clue.
    waiting: Vec<bool>,
    /// Frames where Mario picks something up in the reference, and the
    /// behaviour of what he picks up. Objects like Bob-ombs wander, so the
    /// grab can't be replayed: the miner goes for the object itself.
    pub(crate) pickups: Vec<(usize, u32)>,
    /// Where the star the route collects sits (static), if found: the last
    /// stretch is steered by distance to it.
    pub(crate) star: Option<[f32; 3]>,
}

/// Frames before the end of the reference where the star-distance bonus applies.
pub(crate) const STAR_ZONE: usize = 140;
/// Reference frames of score per world unit closer to the star.
const STAR_WEIGHT: f32 = 2.0;
const STAR_RANGE: f32 = 800.0;

/// The last this many reference frames are matched more strictly.
const FINISH: usize = 200;
const FINISH_VEL_WEIGHT: f32 = 1.5;

/// Frames Mario must stand still for it to count as waiting.
const WAIT_RUN: usize = 15;

fn waiting_runs(snaps: &[Snap]) -> Vec<bool> {
    let mut out = vec![false; snaps.len()];
    let mut i = 0;
    while i < snaps.len() {
        let mut j = i;
        while j < snaps.len() && !snaps[j].moved {
            j += 1;
        }
        if j - i >= WAIT_RUN {
            out[i..j].iter_mut().for_each(|w| *w = true);
        }
        i = j.max(i + 1);
    }
    out
}

/// Replays the route to shortly before its end and finds the bhvStar
/// nearest to where Mario ends up.
fn find_star(route: &[GamePad], end: usize, last: &Snap) -> Option<[f32; 3]> {
    let mut g = sm64pow::generator().ok()?.create_game().ok()?;
    let stop = end.saturating_sub(30);
    for (i, p) in route.iter().enumerate().take(stop + 1) {
        g.step_game(*p).ok()?;
        if i == stop {
            let st = read_state(&g, i as u32 + 1, true);
            let d = |o: &[f32; 3]| (o[0] - last.pos[0]).powi(2) + (o[1] - last.pos[1]).powi(2) + (o[2] - last.pos[2]).powi(2);
            return st.objects?.iter().filter(|o| o.bhv_name() == "bhvStar").map(|o| o.pos).min_by(|a, b| d(a).total_cmp(&d(b)));
        }
    }
    None
}

impl Reference {
    /// Plays `route` unperturbed until the rules' success condition and
    /// records the states. Fails if it never succeeds.
    pub fn record(route: &[GamePad]) -> Result<Reference> {
        let mut g = sm64pow::generator()?.create_game()?;
        let start = sm64pow::game::read_progress(&g)?;
        let rules = sm64pow::rules();
        let mut snaps: Vec<Snap> = Vec::new();
        let mut pickups = Vec::new();
        for (i, p) in route.iter().enumerate() {
            g.step_game(*p)?;
            let mut st = read_state(&g, i as u32 + 1, false);
            if st.holding && snaps.last().is_some_and(|l| !l.holding) {
                st = read_state(&g, i as u32 + 1, true);
                let bhv = st.held_slot.and_then(|slot| st.objects.as_ref()?.iter().find(|o| o.slot == slot).map(|o| o.bhv));
                if let Some(b) = bhv {
                    pickups.push((i, b));
                }
            }
            let s = Snap::new(&st, snaps.last());
            snaps.push(s);
            if rules.is_success(&start, &sm64pow::game::read_progress(&g)?) {
                let waiting = waiting_runs(&snaps);
                let star = find_star(route, i, &snaps[i]);
                return Ok(Reference { inputs: route[..=i].to_vec(), snaps, waiting, pickups, star });
            }
        }
        anyhow::bail!("the reference route never meets the success condition ({} frames)", route.len())
    }

    /// Near the end the route has to be followed closely: the success
    /// condition (touching a star) happens at one exact place.
    fn pos_scale(&self, j: usize) -> f32 {
        if j + FINISH >= self.snaps.len() { 10.0 } else { 25.0 }
    }

    /// The star is hit in mid-air at speed: a state at the right place with
    /// the wrong momentum (a ground pound) is a dead end there.
    fn vel_weight(&self, j: usize) -> f32 {
        if j + FINISH >= self.snaps.len() { FINISH_VEL_WEIGHT } else { 0.25 }
    }

    fn input(&self, i: isize) -> GamePad {
        if i < 0 {
            GamePad::default()
        } else {
            *self.inputs.get(i as usize).unwrap_or(self.inputs.last().unwrap())
        }
    }

    /// Best matching reference index for `s` in `[lo, hi)`: its index,
    /// score (index minus `CLOSENESS` x distance: further along and closer is
    /// better) and distance.
    /// Like [`Reference::best_match`], but only on-track matches count: the
    /// best with distance under `ON_TRACK`, else (score `None`) the closest.
    pub(crate) fn track_match(&self, s: &Snap, lo: usize, hi: usize) -> (Option<(usize, f32)>, f32) {
        let hi = hi.min(self.snaps.len());
        let mut best: Option<(usize, f32)> = None;
        let mut closest = f32::MAX;
        for j in lo..hi {
            if let Some(d) = dist_scaled(s, &self.snaps[j], self.waiting[j], self.pos_scale(j), self.vel_weight(j)) {
                closest = closest.min(d);
                if d < ON_TRACK {
                    let sc = j as f32 - CLOSENESS * d;
                    if best.is_none_or(|(_, b)| sc > b) {
                        best = Some((j, sc));
                    }
                }
            }
        }
        (best, closest)
    }

    pub(crate) fn best_match(&self, s: &Snap, lo: usize, hi: usize) -> Option<(usize, f32, f32)> {
        let hi = hi.min(self.snaps.len());
        let mut best: Option<(usize, f32, f32)> = None;
        for j in lo..hi {
            if let Some(d) = dist_scaled(s, &self.snaps[j], self.waiting[j], self.pos_scale(j), self.vel_weight(j)) {
                let sc = j as f32 - CLOSENESS * d;
                if best.is_none_or(|(_, b, _)| sc > b) {
                    best = Some((j, sc, d));
                }
            }
        }
        best
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub candidates: usize,
    pub horizon: usize,
    pub commit: usize,
    pub max_shift: isize,
    /// Iterations without progress before backtracking.
    pub patience: usize,
    pub seed: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config { candidates: 128, horizon: 90, commit: 8, max_shift: 6, patience: 4, seed: 1 }
    }
}

#[derive(Clone)]
pub(crate) struct Candidate {
    shift: isize,
    /// Per frame: Some(pad) overrides the reference input.
    over: Vec<Option<GamePad>>,
    /// Closed loop instead: push the stick toward the reference position
    /// `lead` frames ahead, with buttons per `buttons`.
    steer: Option<Steer>,
    /// Go for the nearest object running this behaviour and grab it (B
    /// once, within the given distance), then follow the reference.
    grab: Option<(u32, f32)>,
}

#[derive(Clone, Copy)]
struct Steer {
    lead: usize,
    /// 0: none, 1: the reference's, 2: A every `period` frames.
    buttons: u8,
    period: usize,
}

/// The stick that points Mario at `to` from `from`, given the camera yaw
/// (SM64: intendedYaw = atan2s(-stickY, stickX) + camera yaw; x = sin, z = cos).
fn stick_toward(from: [f32; 3], to: [f32; 3], cam_yaw: i16) -> (i8, i8) {
    let (dx, dz) = (to[0] - from[0], to[2] - from[2]);
    if dx * dx + dz * dz < 1.0 {
        return (0, 0);
    }
    let want = dx.atan2(dz);
    let rel = want - cam_yaw as f32 * std::f32::consts::TAU / 65536.0;
    ((80.0 * rel.sin()) as i8, (-80.0 * rel.cos()) as i8)
}

fn random_pad(rng: &mut SmallRng, like: GamePad) -> GamePad {
    let mut p = like;
    match rng.random_range(0..4) {
        0 => {
            p.stick_x = (p.stick_x as i32 + rng.random_range(-24..=24)).clamp(-80, 80) as i8;
            p.stick_y = (p.stick_y as i32 + rng.random_range(-24..=24)).clamp(-80, 80) as i8;
        }
        1 => {
            p.stick_x = rng.random_range(-80..=80);
            p.stick_y = rng.random_range(-80..=80);
        }
        2 => {
            p.button ^= [0x8000u16, 0x4000, 0x2000][rng.random_range(0..3)];
        }
        _ => {
            p = GamePad::default();
        }
    }
    p
}

pub(crate) fn make_candidate(rng: &mut SmallRng, cfg: &Config, idx: usize, reference: &Reference, p: usize) -> Candidate {
    let h = cfg.horizon;
    if idx == 0 {
        return Candidate { shift: 0, over: vec![None; h], steer: None, grab: None };
    }
    // Near a reference pickup: a third of the candidates go for the object.
    if idx % 3 == 2 {
        if let Some(&(_, bhv)) = reference.pickups.iter().find(|(f, _)| *f + 10 >= p && *f <= p + 120) {
            let reach = rng.random_range(60.0..140.0);
            return Candidate { shift: 0, over: vec![None; h], steer: None, grab: Some((bhv, reach)) };
        }
    }
    if idx % 4 == 1 {
        let steer = Steer {
            // Lead 0: toward the star itself, in the last stretch.
            lead: if reference.star.is_some() && p + STAR_ZONE + 60 >= reference.snaps.len() && rng.random_bool(0.4) {
                0
            } else {
                [10, 20, 40, 70][rng.random_range(0..4)]
            },
            buttons: rng.random_range(0..3),
            period: rng.random_range(6..30),
        };
        return Candidate { shift: 0, over: vec![None; h], steer: Some(steer), grab: None };
    }
    if idx % 7 == 3 {
        // Wait a while (neutral stick), then carry on with the reference:
        // the perturbation's pokes land on other frames.
        let k = rng.random_range(2..=48usize).min(h - 1);
        let over = (0..h).map(|t| Some(if t < k { GamePad::default() } else { reference.input((p + t - k) as isize) })).collect();
        return Candidate { shift: 0, over, steer: None, grab: None };
    }
    let shift = if rng.random_bool(0.5) { 0 } else { rng.random_range(-(cfg.max_shift as i64)..=cfg.max_shift as i64) as isize };
    let mut over = vec![None; h];
    // A few mutated stretches, mostly near the start where recovery happens.
    let n = rng.random_range(1..=3);
    for _ in 0..n {
        let start = if rng.random_bool(0.7) { rng.random_range(0..h / 3) } else { rng.random_range(0..h) };
        let len = rng.random_range(1..=24).min(h - start);
        let base = reference.input(p as isize + shift + start as isize);
        let hold = random_pad(rng, base);
        let per_frame = rng.random_bool(0.4);
        for (t, o) in over.iter_mut().enumerate().skip(start).take(len) {
            let b = reference.input(p as isize + shift + t as isize);
            *o = Some(if per_frame { random_pad(rng, b) } else { hold });
        }
    }
    Candidate { shift, over, steer: None, grab: None }
}

pub(crate) struct Outcome {
    pub(crate) score: f32,
    /// Frames to success, if it got there within the horizon.
    pub(crate) success_at: Option<usize>,
    pub(crate) pads: Vec<GamePad>,
}

/// The beam controller only mines from power-on (no stars, no keys).
const POWER_ON: sm64pow::game::Progress = sm64pow::game::Progress { stars: 0, keys: 0, grand_star: false, caps: 0, star_flags: [0; 26] };

pub(crate) fn evaluate(
    game: &mut SM64MiningGame,
    base: &SM64MiningGame,
    base_snap: &Snap,
    cand: &Candidate,
    reference: &Reference,
    p: usize,
    params: &Params,
    frames_done: usize,
) -> Result<Outcome> {
    game.sync_from(base)?;
    let rules = sm64pow::rules();
    let mut prev = *base_snap;
    let mut best = f32::MIN;
    let mut pads = Vec::with_capacity(cand.over.len());
    let lo = p.saturating_sub(LOOK_BACK);
    let hi = p + cand.over.len() + LOOK_AHEAD;
    let mut pressed = false;
    let star = reference.star.filter(|_| p + STAR_ZONE >= reference.snaps.len());
    let mut star_min = f32::MAX;
    for (t, o) in cand.over.iter().enumerate() {
        if frames_done + t >= params.max_frames as usize {
            break;
        }
        let mut want = o.unwrap_or_else(|| reference.input(p as isize + cand.shift + t as isize));
        if let Some(st) = cand.steer {
            let target = match (st.lead, reference.star) {
                (0, Some(sp)) => sp,
                _ => reference.snaps[(p + t + st.lead.max(1)).min(reference.snaps.len() - 1)].pos,
            };
            let (sx, sy) = stick_toward(prev.pos, target, prev.cam_yaw);
            want.stick_x = sx;
            want.stick_y = sy;
            match st.buttons {
                0 => want.button = 0,
                2 => want.button = if t % st.period < 3 { 0x8000 } else { 0 },
                _ => {}
            }
        }
        if let Some((bhv, reach)) = cand.grab {
            if !prev.holding && !pressed {
                // Steer at the nearest such object; punch-grab (B) when close.
                let st = read_state(&game.game, (frames_done + t) as u32, true);
                let target = st.objects.as_ref().and_then(|objs| {
                    objs.iter()
                        .filter(|o| o.bhv == bhv)
                        .map(|o| (((o.pos[0] - st.pos[0]).powi(2) + (o.pos[2] - st.pos[2]).powi(2)).sqrt(), o.pos))
                        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
                });
                if let Some((d, pos)) = target {
                    let (sx, sy) = stick_toward(st.pos, pos, st.cam_yaw);
                    let press = d < reach;
                    pressed |= press;
                    want = GamePad::new(if press { 0x4000 } else { 0 }, sx, sy);
                }
            }
        }
        let played = sm64pow::mine_step(game, want)?;
        pads.push(played);
        if rules.is_success(&POWER_ON, &game.progress()?) {
            return Ok(Outcome { score: f32::MAX - t as f32, success_at: Some(t + 1), pads });
        }
        let st = read_state(&game.game, (frames_done + t + 1) as u32, false);
        let s = Snap::new(&st, Some(&prev));
        prev = s;
        // Progress counts only on the reference's path; off it, the score is
        // how close it gets back (always below any on-track score), so a
        // shortcut along a different path can't look like progress.
        if let Some(sp) = star {
            let d = ((s.pos[0] - sp[0]).powi(2) + (s.pos[1] - sp[1]).powi(2) + (s.pos[2] - sp[2]).powi(2)).sqrt();
            star_min = star_min.min(d);
        }
        let (on, closest) = reference.track_match(&s, lo, hi);
        let sc = match on {
            // A small time cost so that reaching the same point sooner wins.
            Some((_, sc)) => sc - 0.05 * t as f32,
            None => p as f32 - 10_000.0 - CLOSENESS * closest,
        };
        let sc = if star.is_some() { sc + STAR_WEIGHT * (STAR_RANGE - star_min).max(0.0) } else { sc };
        if sc > best {
            best = sc;
        }
    }
    Ok(Outcome { score: best, success_at: None, pads })
}

pub struct Mined {
    pub pads: Vec<GamePad>,
    pub sim_frames: u64,
    pub seconds: f64,
    /// rrt: the keys of the objectives that made the run's path (all stages), for the next
    /// search's guide (OBRRT_GUIDE_KEYS); empty for the other controllers.
    pub keys: Vec<String>,
    /// rrt: the played length at the start of each campaign step (the run's segments, for sharing it).
    pub starts: Vec<usize>,
}

struct Checkpoint {
    state: SM64MiningSave,
    played: usize,
    p: usize,
    snap: Snap,
}

/// Mines one block: returns the played pads (ending on the success frame),
/// or None if `stop` was raised or the time limit can't be met.
pub fn mine(
    seed: &Seed,
    params: &Params,
    reference: &Reference,
    cfg: &Config,
    stop: &AtomicBool,
    mut log: impl FnMut(&str),
) -> Result<Option<Mined>> {
    let t0 = Instant::now();
    let mut base = sm64pow::new_mining_game(seed, params)?;
    let mut played: Vec<GamePad> = Vec::new();
    let mut snap = Snap::new(&read_state(&base.game, 0, false), None);
    let mut p: usize = 0;
    let mut rng = SmallRng::seed_from_u64(cfg.seed ^ ((seed.word(0) as u64) << 20));
    let mut sim_frames = 0u64;
    let mut checkpoints: Vec<Checkpoint> = Vec::new();
    let mut best_p = 0usize;
    let mut stall = 0usize;
    let mut backtracks = 0usize;
    // Escalation: the furthest reference frame ever reached, and how many
    // backtracks since. Stuck spots get a bigger search.
    let mut furthest = 0usize;
    let mut tries_here = 0usize;
    let mut last_log = Instant::now();

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        if played.len() >= params.max_frames as usize {
            log(&format!("out of time at frame {} (limit {})", played.len(), params.max_frames));
            return Ok(None);
        }
        if played.len() % 32 < cfg.commit {
            checkpoints.push(Checkpoint { state: base.save_state(), played: played.len(), p, snap });
            if checkpoints.len() > 64 {
                checkpoints.remove(0);
            }
        }

        let level = tries_here.min(12) / 3; // 0..=4
        let step_cfg = Config {
            candidates: cfg.candidates << level.min(3),
            horizon: (cfg.horizon + 60 * level).min(300),
            ..cfg.clone()
        };
        let cfg = &step_cfg;
        let cands: Vec<Candidate> = (0..cfg.candidates).map(|i| make_candidate(&mut rng, cfg, i, reference, p)).collect();
        let outs: Vec<Outcome> = cands
            .par_iter()
            .map_init(
                || base.fork().expect("copy game"),
                |g, c| evaluate(g, &base, &snap, c, reference, p, params, played.len()).expect("evaluate"),
            )
            .collect();
        sim_frames += outs.iter().map(|o| o.pads.len() as u64).sum::<u64>();
        let best = outs
            .into_iter()
            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap();

        if let Some(n) = best.success_at {
            played.extend_from_slice(&best.pads[..n]);
            log(&format!(
                "success at frame {} ({} backtracks, {:.1} M simulated frames, {:.0} s)",
                played.len(),
                backtracks,
                sim_frames as f64 / 1e6,
                t0.elapsed().as_secs_f64()
            ));
            return Ok(Some(Mined { pads: played, sim_frames, seconds: t0.elapsed().as_secs_f64(), keys: Vec::new(), starts: Vec::new() }));
        }

        // Commit the first frames of the best plan, on the real game.
        let n = cfg.commit.min(best.pads.len());
        for pad in &best.pads[..n] {
            let played_pad = sm64pow::mine_step(&mut base, *pad)?;
            debug_assert_eq!(played_pad, *pad);
            played.push(played_pad);
            let st = read_state(&base.game, played.len() as u32, false);
            snap = Snap::new(&st, Some(&snap));
        }
        let hi = p + n + LOOK_AHEAD;
        // Where are we on the reference? Only trust a close match; when lost,
        // keep the old index and let the stall counter backtrack.
        if let Some((j, _, d)) = reference.best_match(&snap, p.saturating_sub(LOOK_BACK), hi) {
            if d < ON_TRACK {
                // Frozen in place (cutscene, menu, dialog): time is the only
                // clue, so don't let the index run ahead of the frames played.
                p = if snap.moved { j + 1 } else { (j + 1).min(p + n + 2) };
            }
        }

        if p > furthest {
            furthest = p;
            tries_here = 0;
        }
        if p > best_p {
            best_p = p;
            stall = 0;
        } else if snap.moved || played.len() % (4 * cfg.commit) < cfg.commit {
            // Waiting out a cutscene isn't being stuck: count it slowly.
            stall += 1;
        }
        if stall > cfg.patience && crate::sys::env::var("TRACK_DEBUG").is_ok() {
            log(&format!("stall at p {p}: now {:?}", snap));
            for j in [p.saturating_sub(1), p, p + 3, p + 8] {
                if let Some(r) = reference.snaps.get(j) {
                    log(&format!("   ref {j}: {:?} d {:?}", r, dist(&snap, r, reference.waiting[j])));
                }
            }
        }
        if stall > cfg.patience && checkpoints.len() > 1 {
            // Go back further the more often we get stuck here.
            backtracks += 1;
            tries_here += 1;
            let back = (1 + backtracks % 6).min(checkpoints.len() - 1);
            let cp = &checkpoints[checkpoints.len() - 1 - back];
            base.load_state(&cp.state)?;
            played.truncate(cp.played);
            p = cp.p;
            snap = cp.snap;
            best_p = p;
            stall = 0;
            checkpoints.truncate(checkpoints.len() - back);
        }

        if last_log.elapsed().as_secs_f64() > 5.0 {
            last_log = Instant::now();
            log(&format!(
                "frame {:5}  ref {:5}/{}  level {} area {}  backtracks {}  search x{}  {:.1} M sim frames",
                played.len(),
                p,
                reference.snaps.len(),
                snap.phase.level,
                snap.phase.area,
                backtracks,
                1 << (tries_here.min(12) / 3).min(3),
                sim_frames as f64 / 1e6
            ));
        }
    }
}
