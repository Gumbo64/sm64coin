//! sm64coin-automine: mines SM64Coin by playing Super Mario 64 automatically.
//!
//! Loop: fetch a block template from the daemon, build the block's start
//! state (power-on, or the parent's end state: the current cycle's played pads
//! replayed unperturbed, cached per tip), play on from there to the game's
//! next progress point (rh.rs: a route library keyed by the stars already
//! collected), seal the proof with `seal_proof_at`, stamp the block "now" and
//! submit. A winning run longer than the current time limit waits until the
//! limit has grown to it (continuous decay, `sm64_valid_at`) unless the tip
//! changes. A new block on the network restarts the attempt on a fresh template.

use anyhow::{Result, bail};
use clap::Parser;
use sm64coin_miner::{beam, rh, routes::Library, rpc, rrt};
use sm64pow::proof::Start;
use sm64pow::sm64_binds::SaveState;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(about = "Mine SM64Coin by playing Super Mario 64 automatically")]
struct Args {
    /// Address the block reward is paid to (starts with SM64).
    #[arg(long)]
    address: String,
    #[arg(long, default_value = "http://127.0.0.1:16481")]
    daemon: String,
    /// US Super Mario 64 ROM.
    #[arg(long, default_value = "baserom.us.z64")]
    rom: String,
    /// Beam controller only: reference route(s) (.m64) that meet the success condition unperturbed.
    /// The first that works is used; others are fallbacks.
    /// (The chain's own blocks are added too, see --chain-routes; the bomb-boost
    /// routes chomp-*.m64 rarely survive the perturbation, so they're opt-in.)
    #[arg(long, default_values_t = ["pow/miner/routes/block1-human.m64".to_string()])]
    route: Vec<String>,
    /// The controller: `rh` (the rolling-horizon GA over the ETA objectives of
    /// the BoB star routes, rh.rs; the only one that continues from a parent's
    /// end state) or `beam` (the old reference-following beam search; first
    /// star from power-on only).
    #[arg(long, default_value = "rrt")]
    controller: String,
    /// rrt (the seg120 campaign per block): emulated frames (all threads) a castle tree / a course tree may use
    /// before the budget multiplier (campaign.py --hub, --course), a wall-clock cap of one tree (s, 0: none),
    /// checkpoints in a row before a course is given up (--checkpoints), the course trees' stall stop in frames
    /// (--stall, 0: off), more tree settings "VAR=value ..." (--env), scratch directory (also the campaign's
    /// memory across blocks and its roadmap).
    #[arg(long, default_value_t = 60_000_000)]
    rrt_hub_frames: u64,
    #[arg(long, default_value_t = 150_000_000)]
    rrt_course_frames: u64,
    #[arg(long, default_value_t = 0.0)]
    rrt_step_seconds: f64,
    #[arg(long, default_value_t = 8)]
    rrt_checkpoints: usize,
    #[arg(long, default_value_t = 0)]
    rrt_stall: u64,
    #[arg(long, default_value = "")]
    rrt_env: String,
    #[arg(long, default_value = "run/rrt-scratch")]
    rrt_scratch: String,
    /// rh: population, horizon, frames committed per step, threads.
    #[arg(long, default_value_t = 32)]
    rh_pop: usize,
    #[arg(long, default_value_t = 600)]
    rh_horizon: usize,
    #[arg(long, default_value_t = 4)]
    rh_commit: usize,
    #[arg(long, default_value_t = 4)]
    rh_threads: usize,
    /// Live game states in the beam search.
    #[arg(long, default_value_t = 16)]
    beam: usize,
    /// Candidate plans simulated per beam state per step.
    #[arg(long, default_value_t = 24)]
    candidates: usize,
    /// Give up an attempt after this many re-seeds without progress (a new
    /// template is a new chance).
    #[arg(long, default_value_t = 10)]
    give_up: usize,
    #[arg(long, default_value_t = 90)]
    horizon: usize,
    #[arg(long, default_value_t = 8)]
    commit: usize,
    /// Stop after this many blocks (0: forever).
    #[arg(long, default_value_t = 0)]
    blocks: u64,
    /// Beam controller only: also learn routes from the chain (blocks from power-on only; default off).
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    chain_routes: bool,
    /// Where to keep each route's win/attempt record.
    #[arg(long, default_value = "run/automine-routes.json")]
    route_stats: String,
    /// Routes tried per block template (best-ranked first).
    #[arg(long, default_value_t = 3)]
    routes_per_template: usize,
    /// rh: frames a run may take (0: the most any difficulty allows, 36000).
    #[arg(long, default_value_t = 0)]
    rh_frames: usize,
    /// Save each sealed proof's pads here as <height>.m64 (from the block's start state, not from power-on).
    #[arg(long)]
    save_dir: Option<String>,
    /// Testing the hold policy: submit a run that waits for the limit this
    /// many seconds early, timestamped when it becomes valid (nodes accept
    /// stamps up to 120 s ahead; an honest node holds the block until its
    /// own clock lets the run fit). 0: off.
    #[arg(long, default_value_t = 0)]
    stamp_ahead: u64,
    /// Testing the hold policy deterministically: seal the first block and
    /// save it here (JSON) instead of submitting it, then exit.
    #[arg(long)]
    save_block: Option<String>,
    /// With --save-block's file: print the fixed difficulty at which its run
    /// becomes valid `--hold-in` seconds from now (restart the test node with
    /// it), then exit.
    #[arg(long)]
    pick_difficulty: Option<String>,
    /// Seconds from now for --pick-difficulty.
    #[arg(long, default_value_t = 160)]
    hold_in: u64,
    /// With --save-block's file: stamp the block at the time its run fits the
    /// daemon's current limit and submit it now (early, if that is ahead),
    /// then exit.
    #[arg(long)]
    submit_saved: Option<String>,
    /// Added to every search's seed (different runs from the same template, e.g. for tests).
    #[arg(long, default_value_t = 0)]
    salt: u64,
    /// Anytime mining (rrt): a run over the current limit waits for it, and meanwhile fresh
    /// searches from the same start look for a shorter one; the shortest is sent this many
    /// seconds before it is valid (nodes hold it until then; at most the 120 s future limit).
    /// 0: wait idle, the old way.
    #[arg(long, default_value_t = 100)]
    anytime_ahead: u64,
    /// Goal bias for those searches, as RRT samples its goal among random targets: `keys` the
    /// objectives that made the best run's path (their pool weight x --guide-weight, kinship
    /// waived), `points` the best run's positions as waypoint objectives (rrt::write_guide),
    /// `off`. Bench (one block, 5 guided searches paired by seed with unguided ones): keys
    /// 2109/2258/3249/2256/1706 frames vs 2104/2703/4064/2731/2460 unguided; points worse.
    #[arg(long, default_value = "keys")]
    guide: String,
    /// The guide's pool weight (a multiplier for `keys`).
    #[arg(long, default_value_t = 20.0)]
    guide_weight: f64,
    /// Frames between the `points` guide's waypoints.
    #[arg(long, default_value_t = 60)]
    guide_every: usize,
    /// Experiment: N searches on the first template, nothing submitted (each its own tree, the
    /// guide from the best so far when --guide-weight > 0); prints each run's length and the best.
    #[arg(long, default_value_t = 0)]
    anytime_bench: usize,
}

/// A sealed block kept for the hold test (--save-block).
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedBlock {
    template_blob: String,
    proof: String,
    frames: u32,
    parent_ts: u64,
    prev_hash: String,
    height: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// The parent's end state, cached per tip.
struct ParentCache {
    tip: String,
    state: SaveState,
}

/// Smallest `ts` such that a run of `frames` is within the limit of a block
/// timestamped `ts` (never before `from`).
fn valid_from(retarget: u64, parent_ts: u64, frames: u32, from: u64) -> u64 {
    let r = sm64pow::rules();
    let mut ts = from.max(parent_ts);
    // The limit grows with the timestamp; step by doubling then bisect.
    if r.max_frames(retarget, ts - parent_ts) >= frames {
        return ts;
    }
    let mut hi = ts + 1;
    while r.max_frames(retarget, hi - parent_ts) < frames {
        hi = ts + (hi - ts) * 2;
        if hi - parent_ts > 1 << 40 {
            return u64::MAX;
        }
    }
    let mut lo = ts;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if r.max_frames(retarget, mid - parent_ts) >= frames { hi = mid } else { lo = mid }
    }
    ts = hi;
    ts
}

fn main() -> Result<()> {
    let a = Args::parse();
    if !a.address.starts_with("SM6") {
        bail!("{} does not look like an SM64Coin address", a.address);
    }
    sm64pow::init(std::fs::read(&a.rom)?)?;
    let daemon = rpc::Daemon::new(&a.daemon);
    if let Some(f) = &a.pick_difficulty {
        let b: SavedBlock = serde_json::from_str(&std::fs::read_to_string(f)?)?;
        let r = sm64pow::rules();
        let at = now() + a.hold_in;
        // The largest difficulty whose limit still lets the run in at `at`
        // (the limit grows with elapsed time and shrinks with difficulty).
        let fits = |d: u64| r.max_frames(d, at.saturating_sub(b.parent_ts)) >= b.frames;
        let (mut lo, mut hi) = (1u64, u64::MAX);
        if !fits(lo) {
            bail!("{} frames don't fit even at difficulty 1 at +{} s", b.frames, a.hold_in);
        }
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if fits(mid) { lo = mid } else { hi = mid }
        }
        let from = valid_from(lo, b.parent_ts, b.frames, b.parent_ts);
        println!("{lo}");
        eprintln!("difficulty {lo}: {} frames valid at {from} (now +{}), limit now {}", b.frames, from as i64 - now() as i64, r.max_frames(lo, now().saturating_sub(b.parent_ts)));
        return Ok(());
    }
    if let Some(f) = &a.submit_saved {
        let b: SavedBlock = serde_json::from_str(&std::fs::read_to_string(f)?)?;
        let t = daemon.template(&a.address)?;
        if t.prev_hash != b.prev_hash {
            bail!("the tip moved: the saved block is on {}, the daemon's tip is {}", b.prev_hash, t.prev_hash);
        }
        let retarget = t.sm64_retarget_difficulty.max(1);
        let ts = valid_from(retarget, b.parent_ts, b.frames, now());
        let r = sm64pow::rules();
        println!(
            "saved block {}: {} frames; difficulty {retarget}, limit now {}; stamped {ts} = now +{} s",
            b.height, b.frames, r.max_frames(retarget, now().saturating_sub(b.parent_ts)), ts as i64 - now() as i64
        );
        match daemon.submit(&rpc::seal(&b.template_blob, &hex::decode(&b.proof)?, Some(ts))?) {
            Ok(()) => println!("submitted"),
            Err(e) => println!("submit failed: {e}"),
        }
        return Ok(());
    }
    let mut lib = Library::new(Some(a.route_stats.clone()));
    if a.controller == "beam" {
        for r in &a.route {
            if let Err(e) = lib.add_file(r) {
                println!("route {r} skipped: {e}");
            }
        }
        if a.chain_routes {
            match lib.harvest(&daemon) {
                Ok(n) => println!("{n} routes learned from the chain"),
                Err(e) => println!("route harvest failed: {e}"),
            }
        }
        if lib.routes.is_empty() {
            bail!("no usable reference route");
        }
    } else if a.controller == "rrt" {
        println!("rrt coverage:");
        for (point, how) in rrt::coverage() {
            println!("  {point}  <-  {how}");
        }
    } else {
        println!("rh routes:");
        for (point, route) in rh::coverage() {
            println!("  {point}  <-  {route}");
        }
    }
    let mut mined = 0u64;
    let mut attempt = 0u64;
    let mut parent_cache: Option<ParentCache> = None;
    loop {
        attempt += 1;
        let t = match daemon.template(&a.address) {
            Ok(t) => t,
            Err(e) => {
                println!("daemon: {e}; retrying in 5 s");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        let params = sm64pow::params(t.sm64_retarget_difficulty.max(1));
        let seed = sm64pow::Seed::from_hex(&t.sm64_seed_hash)?;
        let elapsed = t.sm64_timestamp.saturating_sub(t.sm64_parent_timestamp);
        let limit_now = sm64pow::rules().max_frames(t.sm64_retarget_difficulty.max(1), elapsed);
        if limit_now != t.sm64_max_frames || params.rng.random_amount != t.sm64_random_amount || params.rng.window_length != t.sm64_window_length {
            bail!(
                "this miner's rules ({}, {} frames) differ from the daemon's ({}, {} frames): rebuild one of them",
                sm64pow::rules().name(),
                limit_now,
                t.sm64_rules,
                t.sm64_max_frames
            );
        }
        println!(
            "\nblock {} on {}..  retarget {}  limit {} frames now  start {}  parent {} stars {} keys  seed {}",
            t.height,
            &t.prev_hash[..12],
            t.sm64_retarget_difficulty,
            t.sm64_max_frames,
            t.sm64_start,
            t.sm64_parent_stars,
            t.sm64_parent_keys,
            &t.sm64_seed_hash[..8]
        );

        // Where the run starts: the parent's end state, or power-on.
        let mut start = if t.sm64_start == "power-on" { Start::PowerOn } else { Start::Parent };
        let mut parent_state: Option<SaveState> = None;
        if start == Start::Parent {
            if parent_cache.as_ref().is_none_or(|c| c.tip != t.prev_hash) {
                match daemon.cycle(Some(&t.prev_hash)).and_then(|c| rpc::cycle_end_state(&c).map(|s| (c, s))) {
                    Ok((c, (state, p))) => {
                        println!("  cycle: {} blocks, {} frames replayed -> {} stars {} keys", c.blocks.len(), c.pads.len(), p.stars, p.keys);
                        parent_cache = Some(ParentCache { tip: t.prev_hash.clone(), state });
                    }
                    Err(e) => {
                        println!("  cannot build the parent's end state: {e:#}; retrying in 5 s");
                        std::thread::sleep(Duration::from_secs(5));
                        continue;
                    }
                }
            }
            parent_state = parent_cache.as_ref().map(|c| c.state.clone());
        }

        if a.anytime_bench > 0 {
            return anytime_bench(&a, start, parent_state.as_ref(), &seed, &params, attempt);
        }

        // Watch for a new block while playing.
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (stop, done) = (stop.clone(), done.clone());
            let (url, prev) = (a.daemon.clone(), t.prev_hash.clone());
            // A parent-only template becomes "either" at sm64_power_on_at.
            let power_on_at = if t.sm64_start == "parent" { t.sm64_power_on_at } else { 0 };
            std::thread::spawn(move || {
                let d = rpc::Daemon::new(&url);
                while !done.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(2));
                    if power_on_at != 0 && now() >= power_on_at {
                        println!("  the stall time has passed: restarting to allow the power-on start");
                        stop.store(true, Ordering::Relaxed);
                        return;
                    }
                    if let Ok(h) = d.top_hash() {
                        if h != prev {
                            println!("  new block {}.. on the network: restarting", &h[..12]);
                            stop.store(true, Ordering::Relaxed);
                            return;
                        }
                    }
                }
            })
        };

        let mut result = None;
        let mut no_route = None;
        if a.controller == "rrt" || a.controller == "rh" {
            let o = rh::RhOptions { pop: a.rh_pop, horizon: a.rh_horizon, commit: a.rh_commit, threads: a.rh_threads, seed: attempt + a.salt, frame_cap: a.rh_frames, max_retries: if t.sm64_start == "either" { 3 } else { 40 }, ..Default::default() };
            let ro = rrt::RrtOptions { threads: a.rh_threads, hub_frames: a.rrt_hub_frames, course_frames: a.rrt_course_frames, step_seconds: a.rrt_step_seconds, checkpoints: a.rrt_checkpoints, stall: a.rrt_stall, extra_env: a.rrt_env.clone(), seed: attempt.wrapping_mul(7919) + a.salt, frame_cap: a.rh_frames, scratch: a.rrt_scratch.clone().into(), rom: Some(a.rom.clone()), ..Default::default() };
            let use_rrt = a.controller == "rrt";
            let mut r = if use_rrt { rrt::mine(start, parent_state.as_ref(), &seed, &params, &ro, &stop, |m| println!("  {m}"))? } else { rh::mine(start, parent_state.as_ref(), &seed, &params, &o, &stop, |m| println!("  {m}"))? };
            let fallback = match &r {
                rh::Attempt::NoRoute(why) => Some(format!("no route from the parent's end state: {why}")),
                rh::Attempt::Gave(why) if t.sm64_start == "either" && !stop.load(Ordering::Relaxed) => Some(format!("the parent's end state didn't give progress quickly ({why})")),
                _ => None,
            };
            if let Some(why) = fallback {
                println!("  {why}");
                if t.sm64_start == "either" {
                    println!("  taking the power-on start");
                    start = Start::PowerOn;
                    parent_state = None;
                    r = if use_rrt { rrt::mine(start, None, &seed, &params, &ro, &stop, |m| println!("  {m}"))? } else { rh::mine(start, None, &seed, &params, &o, &stop, |m| println!("  {m}"))? };
                }
            }
            match r {
                rh::Attempt::Found(m) => result = Some(m),
                rh::Attempt::Gave(why) => println!("  no progress: {why}"),
                rh::Attempt::NoRoute(why) => no_route = Some(why),
            }
        } else if start == Start::PowerOn || t.sm64_parent_stars + t.sm64_parent_keys == 0 {
            for (k, i) in lib.ranked().into_iter().take(a.routes_per_template.max(1)).enumerate() {
                let reached = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let cfg = beam::BeamConfig {
                    width: a.beam,
                    per_state: a.candidates,
                    horizon: a.horizon,
                    commit: a.commit,
                    give_up: a.give_up,
                    seed: attempt * 1000 + k as u64,
                    reached: Some(reached.clone()),
                    ..Default::default()
                };
                let route = &lib.routes[i];
                println!("  following route {} ({} frames)", route.name, route.reference.inputs.len());
                let len = route.reference.inputs.len();
                result = beam::mine(&seed, &params, &route.reference, &cfg, &stop, |m| println!("  {m}"))?;
                if stop.load(Ordering::Relaxed) && result.is_none() {
                    break; // interrupted by a new block: not the route's fault
                }
                let progress = reached.load(Ordering::Relaxed) as f64 / len.max(1) as f64;
                lib.record(i, result.is_some(), progress);
                if result.is_some() {
                    break;
                }
            }
            start = Start::PowerOn;
        } else {
            no_route = Some("the beam controller only starts from power-on".into());
        }
        done.store(true, Ordering::Relaxed);
        let _ = watcher.join();
        let Some(m) = result else {
            if let Some(why) = no_route {
                // Nothing to play from here: wait for a new tip or the restart
                // delay (after which the template allows the power-on start).
                let wake = if t.sm64_power_on_at > now() { t.sm64_power_on_at } else { now() + 5 };
                println!("  nothing to try ({why}); power-on start allowed at {} (in {} s)", t.sm64_power_on_at, wake.saturating_sub(now()));
                while now() < wake.min(now() + 3600) {
                    std::thread::sleep(Duration::from_secs(2));
                    if daemon.top_hash().map(|h| h != t.prev_hash).unwrap_or(false) {
                        break;
                    }
                    if now() >= wake {
                        break;
                    }
                }
            }
            continue;
        };

        // The block stores only the non-poked frames' inputs (pokes are
        // regenerated from the seed).
        let frames = m.pads.len() as u32;
        let proof = sm64pow::seal_proof_at(start, parent_state.as_ref(), &seed, &params, &m.pads)?;
        match sm64pow::decode_proof_any(&proof, parent_state.as_ref(), &seed, &params) {
            Ok(pr) if pr.frames == frames && pr.start == start => {
                let again = sm64pow::played_from_proof_at(parent_state.as_ref(), &seed, &params, &pr)?;
                if again != m.pads {
                    println!("  BUG: proof does not replay to the run's pads");
                    continue;
                }
                println!("  proof sealed: {frames} frames, {} bytes, {} inputs, start {:?}", proof.len(), pr.inputs.len(), start);
            }
            other => {
                println!("  BUG: own proof does not decode: {:?}", other.map(|p| p.frames));
                continue;
            }
        }
        if let Some(dir) = &a.save_dir {
            std::fs::create_dir_all(dir)?;
            let mut m64 = vec![0u8; 0x400];
            m64[..4].copy_from_slice(b"M64\x1a");
            for p in &m.pads {
                m64.extend_from_slice(&p.button.to_be_bytes());
                m64.push(p.stick_x as u8);
                m64.push(p.stick_y as u8);
            }
            std::fs::write(format!("{dir}/{}.m64", t.height), m64)?;
        }

        if let Some(f) = &a.save_block {
            let b = SavedBlock { template_blob: t.blocktemplate_blob.clone(), proof: hex::encode(&proof), frames, parent_ts: t.sm64_parent_timestamp, prev_hash: t.prev_hash.clone(), height: t.height };
            std::fs::write(f, serde_json::to_string(&b)?)?;
            println!("  saved block {} ({frames} frames) to {f}, not submitted", t.height);
            return Ok(());
        }
        // Anytime mining: over the limit now, so search again (fresh trees, the guide from the
        // best run) until shortly before the best run is valid; keep the shortest.
        let (m, proof, frames) = if a.controller == "rrt" && a.anytime_ahead > 0 && a.stamp_ahead == 0 {
            match improve(&a, &daemon, &t, start, parent_state.as_ref(), &seed, &params, attempt, m, proof)? {
                Some(x) => x,
                None => continue, // a new block landed: the run is void
            }
        } else {
            (m, proof, frames)
        };
        // Within the time limit now? Otherwise wait until it has grown to
        // this run's length (the tip changing abandons it).
        let retarget = t.sm64_retarget_difficulty.max(1);
        // A restart (power-on start on a parent that didn't end the cycle) is
        // judged against the limit `sm64_restart_delay` seconds earlier.
        let restart = start == Start::PowerOn && t.sm64_start == "either";
        let delay = if restart { t.sm64_restart_delay } else { 0 };
        let mut ts = valid_from(retarget, t.sm64_parent_timestamp + delay, frames, now());
        if ts > now() {
            let asked = daemon.template_valid_at(&a.address, frames).map(|x| x.sm64_valid_at).unwrap_or(0);
            let asked = if asked == 0 { 0 } else { asked + delay };
            ts = ts.max(asked);
            println!("  {frames} frames is over the current limit ({}); valid at {ts} (in {} s, daemon says {asked})", sm64pow::rules().max_frames(retarget, now().saturating_sub(t.sm64_parent_timestamp)), ts.saturating_sub(now()));
            let mut abandoned = false;
            if a.stamp_ahead > 0 {
                println!("  test: submitting {} s early, stamped {ts}", a.stamp_ahead.min(ts.saturating_sub(now())));
            }
            let ahead = if a.stamp_ahead > 0 { a.stamp_ahead } else if a.controller == "rrt" { a.anytime_ahead.min(110) } else { 0 };
            while now() + ahead < ts {
                std::thread::sleep(Duration::from_secs(1));
                if now() % 3 == 0 && daemon.top_hash().map(|h| h != t.prev_hash).unwrap_or(false) {
                    println!("  new block on the network: abandoning the waiting run");
                    abandoned = true;
                    break;
                }
            }
            if abandoned {
                continue;
            }
            if ahead == 0 {
                ts = ts.max(now());
            }
        } else {
            ts = now();
        }
        match daemon.submit(&rpc::seal(&t.blocktemplate_blob, &proof, Some(ts))?) {
            Ok(()) => {
                mined += 1;
                // Stamped ahead of the clock: the node holds it until its run fits.
                let held = ts > now();
                println!(
                    "  BLOCK {} {} ({:.6} SM64 reward, {} frames = {:.1} s of play, {:.0} s to find)",
                    t.height,
                    if held { format!("SENT, held by the node for {} s until it fits", ts - now()) } else { "ACCEPTED".into() },
                    t.expected_reward as f64 / 1e12,
                    m.pads.len(),
                    m.pads.len() as f64 / 30.0,
                    m.seconds
                );
                if a.blocks != 0 && mined >= a.blocks {
                    return Ok(());
                }
            }
            Err(e) => println!("  block rejected: {e}"),
        }
    }
}

/// The rrt options for search `k` of a block (its seed), with the guide file if any.
fn rrt_options(a: &Args, attempt: u64, k: u64, guide: Option<&str>) -> rrt::RrtOptions {
    let mut env = a.rrt_env.clone();
    if let Some(g) = guide {
        env = format!("{env} {g}");
    }
    rrt::RrtOptions {
        threads: a.rh_threads,
        hub_frames: a.rrt_hub_frames,
        course_frames: a.rrt_course_frames,
        step_seconds: a.rrt_step_seconds,
        checkpoints: a.rrt_checkpoints,
        stall: a.rrt_stall,
        extra_env: env,
        seed: attempt.wrapping_mul(7919).wrapping_add(k.wrapping_mul(104729)) + a.salt,
        frame_cap: a.rh_frames,
        scratch: a.rrt_scratch.clone().into(),
        rom: Some(a.rom.clone()),
        ..Default::default()
    }
}

/// Writes the guide from the best run `m` (--guide) and returns the search's env for it.
fn guide_from(a: &Args, start: Start, parent: Option<&SaveState>, seed: &sm64pow::Seed, params: &sm64pow::Params, m: &rh::Mined) -> Option<String> {
    if a.guide_weight <= 0.0 || a.guide == "off" {
        return None;
    }
    let _ = std::fs::create_dir_all(&a.rrt_scratch);
    let dir = std::path::Path::new(&a.rrt_scratch);
    if a.guide == "keys" {
        if m.keys.is_empty() {
            return None;
        }
        let path = dir.join("guide_keys.txt");
        let mut uniq = m.keys.clone();
        uniq.sort();
        uniq.dedup();
        std::fs::write(&path, uniq.join("\n")).ok()?;
        println!("  guide: {} objectives from the best run's path (weight x{}, kinship waived)", uniq.len(), a.guide_weight);
        return Some(format!("OBRRT_GUIDE_KEYS={} OBRRT_GUIDE_WEIGHT={}", path.display(), a.guide_weight));
    }
    let path = dir.join("guide.json");
    match rrt::write_guide(start, parent, seed, params, &m.pads, a.guide_every.max(1), a.guide_weight, &path) {
        Ok(n) if n > 0 => {
            println!("  guide: {n} waypoints from the best run (weight {})", a.guide_weight);
            Some(format!("OBRRT_SPEC={}", path.display()))
        }
        Ok(_) => None,
        Err(e) => {
            println!("  guide: {e:#}");
            None
        }
    }
}

/// Anytime mining for a run `m` (sealed as `proof`) that is over the current limit: fresh
/// searches from the same start until `anytime_ahead` s before the best run is valid. A shorter
/// run replaces the best (it is valid sooner); one valid now ends the search. Returns the best
/// (run, proof, frames), or None if a new block landed (the run is void).
#[allow(clippy::too_many_arguments)]
fn improve(a: &Args, _daemon: &rpc::Daemon, t: &rpc::Template, start: Start, parent: Option<&SaveState>, seed: &sm64pow::Seed, params: &sm64pow::Params, attempt: u64, m: rh::Mined, proof: Vec<u8>) -> Result<Option<(rh::Mined, Vec<u8>, u32)>> {
    let retarget = t.sm64_retarget_difficulty.max(1);
    let restart = start == Start::PowerOn && t.sm64_start == "either";
    let delay = if restart { t.sm64_restart_delay } else { 0 };
    let valid = |frames: u32| valid_from(retarget, t.sm64_parent_timestamp + delay, frames, now());
    let mut best = (m, proof);
    let mut best_frames = best.0.pads.len() as u32;
    let mut k = 1u64;
    let ahead = a.anytime_ahead.min(110);
    while valid(best_frames) > now() + ahead {
        let deadline = valid(best_frames) - ahead;
        println!("  anytime: best {best_frames} frames, valid in {} s; search {k} for a shorter run until then", valid(best_frames).saturating_sub(now()));
        let guide = guide_from(a, start, parent, seed, params, &best.0);
        let ro = rrt_options(a, attempt, k, guide.as_deref());
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let new_tip = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (stop, done, new_tip) = (stop.clone(), done.clone(), new_tip.clone());
            let (url, prev) = (a.daemon.clone(), t.prev_hash.clone());
            std::thread::spawn(move || {
                let d = rpc::Daemon::new(&url);
                while !done.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(1));
                    if now() >= deadline {
                        stop.store(true, Ordering::Relaxed);
                        return;
                    }
                    if d.top_hash().map(|h| h != prev).unwrap_or(false) {
                        new_tip.store(true, Ordering::Relaxed);
                        stop.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            })
        };
        let r = rrt::mine(start, parent, seed, params, &ro, &stop, |m| println!("    {m}"));
        done.store(true, Ordering::Relaxed);
        let _ = watcher.join();
        if new_tip.load(Ordering::Relaxed) {
            println!("  anytime: a new block landed: the run is void");
            return Ok(None);
        }
        match r? {
            rh::Attempt::Found(m2) if (m2.pads.len() as u32) < best_frames => {
                let f2 = m2.pads.len() as u32;
                let p2 = sm64pow::seal_proof_at(start, parent, seed, params, &m2.pads)?;
                println!("  anytime: search {k} found {f2} frames (was {best_frames}): valid {} s sooner", valid(best_frames).saturating_sub(valid(f2)));
                best = (m2, p2);
                best_frames = f2;
            }
            rh::Attempt::Found(m2) => println!("  anytime: search {k} found {} frames, not shorter", m2.pads.len()),
            _ => {}
        }
        if stop.load(Ordering::Relaxed) {
            break; // the deadline
        }
        k += 1;
    }
    Ok(Some((best.0, best.1, best_frames)))
}

/// --anytime-bench: N searches on this template, nothing submitted.
fn anytime_bench(a: &Args, start: Start, parent: Option<&SaveState>, seed: &sm64pow::Seed, params: &sm64pow::Params, attempt: u64) -> Result<()> {
    let never = Arc::new(AtomicBool::new(false));
    let mut best: Option<rh::Mined> = None;
    let t0 = std::time::Instant::now();
    for k in 0..a.anytime_bench as u64 {
        let guide = best.as_ref().and_then(|m| guide_from(a, start, parent, seed, params, m));
        let ro = rrt_options(a, attempt, k, guide.as_deref());
        let t1 = std::time::Instant::now();
        let r = rrt::mine(start, parent, seed, params, &ro, &never, |m| println!("    {m}"))?;
        let secs = t1.elapsed().as_secs_f64();
        match r {
            rh::Attempt::Found(m) => {
                let f = m.pads.len();
                println!("bench search {k}: {f} frames in {secs:.0} s; best {} (total {:.0} s, guide {}, {} path objectives)", best.as_ref().map_or(f, |b: &rh::Mined| b.pads.len().min(f)), t0.elapsed().as_secs_f64(), if guide.is_some() { a.guide.as_str() } else { "none" }, m.keys.len());
                if best.as_ref().is_none_or(|b| f < b.pads.len()) {
                    best = Some(m);
                }
            }
            other => println!("bench search {k}: no run ({}) in {secs:.0} s", match other { rh::Attempt::Gave(w) | rh::Attempt::NoRoute(w) => w, _ => String::new() }),
        }
    }
    Ok(())
}
