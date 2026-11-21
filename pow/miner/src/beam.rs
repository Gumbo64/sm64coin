//! Beam search over the perturbed game, following a reference route.
//!
//! Like [`crate::tracker::mine`] (same candidates, same "progress on the
//! reference's path" score), but instead of one committed line plus
//! backtracking it keeps a beam of game states: each step, every state is
//! expanded with candidate continuations simulated through the real
//! perturbation, and the best distinct children (first `commit` frames of a
//! candidate) form the next beam. Alternatives stay alive, so one bad poke
//! doesn't need luck to recover from. Stagnation re-seeds the beam from an
//! archive of the best states per stretch of the route.

use crate::tracker::{Config, Mined, Reference, Snap, evaluate, make_candidate, LOOK_AHEAD, LOOK_BACK, ON_TRACK};
use anyhow::Result;
use obrrt_core::read_state;
use rand::{SeedableRng, rngs::SmallRng};
use rayon::prelude::*;
use sm64_binds::{GamePad, SM64Game};
use sm64pow::{Params, Seed};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct BeamConfig {
    /// Live states.
    pub width: usize,
    /// Candidates per state per step.
    pub per_state: usize,
    pub horizon: usize,
    pub commit: usize,
    /// Most children one state may have in the next beam.
    pub max_children: usize,
    /// Steps without a new furthest point before re-seeding from the archive.
    pub patience: usize,
    /// Re-seeds without a new furthest point before giving up on this
    /// attempt (proof of work: a fresh template is a fresh chance).
    pub give_up: usize,
    /// Start from these already-played pads instead of power-on (for
    /// experiments; they must be valid for the seed).
    pub start: Vec<GamePad>,
    /// Write the best state's pads here when it first reaches this
    /// reference frame (for experiments).
    pub dump_at: Option<(usize, String)>,
    /// Updated with the furthest reference frame reached (for route stats).
    pub reached: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    pub seed: u64,
}

impl Default for BeamConfig {
    fn default() -> Self {
        BeamConfig { width: 16, per_state: 24, horizon: 90, commit: 8, max_children: 4, patience: 30, give_up: 10, start: vec![], dump_at: None, reached: None, seed: 1 }
    }
}

struct Node {
    game: SM64Game,
    played: Vec<GamePad>,
    snap: Snap,
    /// Reference frame this state is at.
    p: usize,
    score: f32,
    /// Whether the state matches the reference closely at `p` (a lost state
    /// keeps its old `p` and is a poor archive or progress candidate).
    on: bool,
}

fn locate(reference: &Reference, snap: &Snap, p: usize, n: usize) -> (usize, bool) {
    match reference.best_match(snap, p.saturating_sub(LOOK_BACK), p + n + LOOK_AHEAD) {
        Some((j, _, d)) if d < ON_TRACK => (if snap.moved { j + 1 } else { (j + 1).min(p + n + 2) }, true),
        _ => (p, false),
    }
}

/// Mines one block: returns the played pads (ending on the success frame),
/// or None if `stop` was raised or no state can meet the time limit.
pub fn mine(
    seed: &Seed,
    params: &Params,
    reference: &Reference,
    cfg: &BeamConfig,
    stop: &AtomicBool,
    mut log: impl FnMut(&str),
) -> Result<Option<Mined>> {
    let t0 = Instant::now();
    let generator = sm64pow::generator()?;
    let mut root = sm64pow::new_mining_game(seed, params)?;
    let mut snap = Snap::new(&read_state(&root, 0, false), None);
    let mut p0 = 0;
    for (i, pad) in cfg.start.iter().enumerate() {
        let got = sm64pow::mine_step(&mut root, seed, params, i, *pad)?;
        anyhow::ensure!(got == *pad, "start pads don't match the perturbation at frame {i}");
        snap = Snap::new(&read_state(&root, i as u32 + 1, false), Some(&snap));
        p0 = locate(reference, &snap, p0, 1).0;
    }
    let mut beam = vec![Node { game: root, played: cfg.start.clone(), snap, p: p0, score: 0.0, on: true }];
    let mut dumped = false;
    // Best state seen per 64-frame stretch of the reference.
    let mut archive: Vec<Option<Node>> = (0..reference.snaps.len() / 64 + 2).map(|_| None).collect();
    let tcfg = Config { candidates: cfg.per_state, horizon: cfg.horizon, commit: cfg.commit, ..Default::default() };
    let mut rng = SmallRng::seed_from_u64(cfg.seed ^ ((seed.word(0) as u64) << 20));
    let (mut sim_frames, mut furthest, mut since, mut reseeds, mut step) = (0u64, 0usize, 0usize, 0usize, 0usize);
    let mut last_log = Instant::now();
    let mut stuck_reseeds = 0usize;
    let mut credited = 0usize;

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        beam.retain(|n| n.played.len() < params.max_frames as usize);
        if beam.is_empty() {
            log("every state ran out of time");
            return Ok(None);
        }
        step += 1;
        // Escalate at a hard spot: more candidates and a longer horizon the
        // longer the beam has been stuck (reverts as soon as it progresses).
        let level = (stuck_reseeds / 3).min(3);
        let per_state = cfg.per_state << level.min(2);
        let tcfg_now = Config { horizon: (cfg.horizon + 45 * level).min(270), ..tcfg.clone() };
        // Candidates for every state (drawn here so the RNG stays sequential).
        let cands: Vec<Vec<_>> = beam
            .iter()
            .map(|n| (0..per_state).map(|i| make_candidate(&mut rng, &tcfg_now, i, reference, n.p)).collect())
            .collect();
        // Expand: one worker game per state, synced from it for each candidate.
        let outs: Vec<Vec<_>> = beam
            .par_iter()
            .zip(cands.par_iter())
            .map(|(n, cs)| {
                let mut g = generator.copy_from(&n.game).expect("copy game");
                cs.iter()
                    .map(|c| evaluate(seed, &mut g, &n.game, &n.snap, c, reference, n.p, params, n.played.len()).expect("evaluate"))
                    .collect()
            })
            .collect();
        sim_frames += outs.iter().flatten().map(|o| o.pads.len() as u64).sum::<u64>();

        // Success anywhere ends the search.
        for (n, os) in beam.iter().zip(&outs) {
            if let Some(o) = os.iter().filter(|o| o.success_at.is_some()).min_by_key(|o| o.success_at) {
                let mut pads = n.played.clone();
                pads.extend_from_slice(&o.pads[..o.success_at.unwrap()]);
                log(&format!(
                    "success at frame {} ({} steps, {} reseeds, {:.1} M simulated frames, {:.0} s)",
                    pads.len(),
                    step,
                    reseeds,
                    sim_frames as f64 / 1e6,
                    t0.elapsed().as_secs_f64()
                ));
                return Ok(Some(Mined { pads, sim_frames, seconds: t0.elapsed().as_secs_f64() }));
            }
        }

        // Pick the next beam: best candidates, at most max_children per state,
        // skipping near-duplicates.
        let mut ranked: Vec<(f32, usize, usize)> = outs
            .iter()
            .enumerate()
            .flat_map(|(i, os)| os.iter().enumerate().map(move |(k, o)| (o.score, i, k)))
            .collect();
        ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut children = vec![0usize; beam.len()];
        let mut chosen: Vec<(usize, usize, f32)> = Vec::new();
        let mut seen: Vec<Vec<GamePad>> = Vec::new();
        for (score, i, k) in ranked {
            if chosen.len() >= cfg.width {
                break;
            }
            if children[i] >= cfg.max_children {
                continue;
            }
            let o = &outs[i][k];
            let head: Vec<GamePad> = o.pads[..cfg.commit.min(o.pads.len())].to_vec();
            if chosen.iter().zip(&seen).any(|(c, h)| c.0 == i && *h == head) {
                continue;
            }
            children[i] += 1;
            seen.push(head);
            chosen.push((i, k, score));
        }
        // Materialize the children (copy the parent, replay the committed frames).
        let next: Vec<Node> = chosen
            .par_iter()
            .map(|&(i, k, score)| {
                let parent = &beam[i];
                let o = &outs[i][k];
                let n = cfg.commit.min(o.pads.len());
                let mut game = generator.copy_from(&parent.game).expect("copy game");
                let mut played = parent.played.clone();
                let mut snap = parent.snap;
                for pad in &o.pads[..n] {
                    let got = sm64pow::mine_step(&mut game, seed, params, played.len(), *pad).expect("step");
                    debug_assert_eq!(got, *pad);
                    played.push(got);
                    snap = Snap::new(&read_state(&game, played.len() as u32, false), Some(&snap));
                }
                let (p, on) = locate(reference, &snap, parent.p, n);
                Node { game, played, snap, p, score, on }
            })
            .collect();
        beam = next;

        // Archive the best state per stretch; watch for stagnation.
        let top = beam.iter().filter(|n| n.on).map(|n| n.p).max().unwrap_or(0);
        if let Some((at, path)) = &cfg.dump_at {
            if !dumped && top >= *at {
                dumped = true;
                let best = beam.iter().filter(|n| n.on).max_by_key(|n| n.p).unwrap();
                std::fs::write(path, sm64pow::proof::encode_with(&sm64pow::proof::Raw, &best.played))?;
                log(&format!("dumped {} pads at reference frame {} to {path}", best.played.len(), best.p));
            }
        }
        for n in beam.iter().filter(|n| n.on) {
            let b = n.p / 64;
            if archive[b].as_ref().is_none_or(|a| n.score > a.score && n.played.len() <= a.played.len() + 64) {
                archive[b] = Some(Node {
                    game: generator.copy_from(&n.game)?,
                    played: n.played.clone(),
                    snap: n.snap,
                    p: n.p,
                    score: n.score,
                    on: true,
                });
            }
        }
        if top > furthest {
            furthest = top;
            if let Some(r) = &cfg.reached {
                r.store(furthest, std::sync::atomic::Ordering::Relaxed);
            }
        }
        // Real progress (not a one-frame creep) clears the stagnation counters.
        if top > credited + 24 {
            credited = top;
            since = 0;
            stuck_reseeds = 0;
        } else {
            since += 1;
        }
        if since > cfg.patience {
            // Re-seed half the beam from archived states a few stretches back.
            reseeds += 1;
            stuck_reseeds += 1;
            since = 0;
            if std::env::var("TRACK_DEBUG").is_ok() {
                let best = beam.iter().max_by_key(|n| n.p).unwrap();
                if let Some(sp) = reference.star {
                    let d = |n: &Node| ((n.snap.pos[0] - sp[0]).powi(2) + (n.snap.pos[1] - sp[1]).powi(2) + (n.snap.pos[2] - sp[2]).powi(2)).sqrt();
                    log(&format!("reseed: p {} star dist: best {:.0}, closest in beam {:.0}", best.p, d(best), beam.iter().map(d).fold(f32::MAX, f32::min)));
                }
                log(&format!("reseed: best now {:?}", best.snap));
                for j in [best.p.saturating_sub(1), best.p, best.p + 5, best.p + 15] {
                    if let Some(r) = reference.snaps.get(j) {
                        log(&format!("   ref {j}: {:?}", r));
                    }
                }
            }
            if stuck_reseeds > cfg.give_up * 2 {
                log(&format!("stuck at reference frame {furthest} after {} re-seeds: giving up this attempt", cfg.give_up * 2));
                return Ok(None);
            }
            let b = furthest / 64;
            let back = 1 + reseeds % 4;
            let from = b.saturating_sub(back);
            for (slot, a) in (from..=b).rev().filter_map(|i| archive[i].as_ref()).take(cfg.width / 2).enumerate() {
                let node = Node { game: generator.copy_from(&a.game)?, played: a.played.clone(), snap: a.snap, p: a.p, score: a.score, on: true };
                let idx = beam.len() - 1 - slot.min(beam.len() - 1);
                beam[idx] = node;
            }
        }

        if last_log.elapsed().as_secs_f64() > 5.0 {
            last_log = Instant::now();
            let best = beam.iter().max_by_key(|n| n.p).unwrap();
            log(&format!(
                "step {step:4}  ref {:5}/{} at frame {:5}  level {} area {}  beam {}  reseeds {}  {:.1} M sim frames",
                best.p,
                reference.snaps.len(),
                best.played.len(),
                best.snap.phase.level,
                best.snap.phase.area,
                beam.len(),
                reseeds,
                sim_frames as f64 / 1e6
            ));
        }
    }
}
