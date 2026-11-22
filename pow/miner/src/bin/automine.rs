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
    /// On-chain leaderboard opt-in: a display name (max 24 bytes), written with the address's
    /// private view key into the coinbase of every block this miner mines. The explorer then
    /// lists you from the chain alone. The view key is PUBLIC from then on (it shows all payments
    /// to this address forever): use a dedicated mining address.
    #[arg(long)]
    leaderboard_name: Option<String>,
    /// The address's private view key (64 hex). Prefer --leaderboard-view-key-file or the env
    /// variable: command lines are visible to other users.
    #[arg(long)]
    leaderboard_view_key: Option<String>,
    /// File holding the private view key (hex).
    #[arg(long)]
    leaderboard_view_key_file: Option<String>,
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
    /// rrt: emulated frames (all threads) a castle tree / a course tree may use, wall-clock cap of one tree (s),
    /// courses tried from the castle per block, scratch directory, ROM path for the tree processes.
    #[arg(long, default_value_t = 16_000_000)]
    rrt_hub_frames: u64,
    #[arg(long, default_value_t = 60_000_000)]
    rrt_course_frames: u64,
    #[arg(long, default_value_t = 900.0)]
    rrt_step_seconds: f64,
    #[arg(long, default_value_t = 3)]
    rrt_targets: usize,
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
    /// rh: frames a run may take (0: the most any difficulty allows, 18000).
    #[arg(long, default_value_t = 0)]
    rh_frames: usize,
    /// Save each sealed proof's pads here as <height>.m64 (from the block's start state, not from power-on).
    #[arg(long)]
    save_dir: Option<String>,
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
    let mut daemon = rpc::Daemon::new(&a.daemon);
    if let Some(name) = &a.leaderboard_name {
        let vk = match (&a.leaderboard_view_key_file, &a.leaderboard_view_key) {
            (Some(f), _) => std::fs::read_to_string(f)?,
            (None, Some(k)) => k.clone(),
            _ => std::env::var("SM64_LEADERBOARD_VIEW_KEY").map_err(|_| anyhow::anyhow!("--leaderboard-name needs --leaderboard-view-key-file (or the SM64_LEADERBOARD_VIEW_KEY variable)"))?,
        };
        // checks that the view key belongs to the address (A = a*G)
        let tag = sm64lb::tag::Tag::new(name, &a.address, &vk).map_err(|e| anyhow::anyhow!("leaderboard tag: {e}"))?;
        println!("on-chain leaderboard: blocks carry the name {:?} (view key published in each block)", tag.name);
        daemon = daemon.with_extra_nonce(tag.encode());
    } else if a.leaderboard_view_key_file.is_some() || a.leaderboard_view_key.is_some() {
        bail!("a leaderboard view key without --leaderboard-name");
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
            let o = rh::RhOptions { pop: a.rh_pop, horizon: a.rh_horizon, commit: a.rh_commit, threads: a.rh_threads, seed: attempt, frame_cap: a.rh_frames, max_retries: if t.sm64_start == "either" { 3 } else { 40 }, ..Default::default() };
            let ro = rrt::RrtOptions { threads: a.rh_threads, hub_frames: a.rrt_hub_frames, course_frames: a.rrt_course_frames, step_seconds: a.rrt_step_seconds, max_targets: a.rrt_targets, seed: attempt, frame_cap: a.rh_frames, scratch: a.rrt_scratch.clone().into(), rom: Some(a.rom.clone()), ..Default::default() };
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
                // Nothing to play from here: wait for a new tip or the stall
                // time (after which the template allows the power-on start).
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

        // Within the time limit now? Otherwise wait until it has grown to
        // this run's length (the tip changing abandons it).
        let retarget = t.sm64_retarget_difficulty.max(1);
        let mut ts = valid_from(retarget, t.sm64_parent_timestamp, frames, now());
        if ts > now() {
            let asked = daemon.template_valid_at(&a.address, frames).map(|x| x.sm64_valid_at).unwrap_or(0);
            ts = ts.max(asked);
            println!("  {frames} frames is over the current limit ({}); valid at {ts} (in {} s, daemon says {asked})", sm64pow::rules().max_frames(retarget, now().saturating_sub(t.sm64_parent_timestamp)), ts.saturating_sub(now()));
            let mut abandoned = false;
            while now() < ts {
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
            ts = ts.max(now());
        } else {
            ts = now();
        }
        match daemon.submit(&rpc::seal(&t.blocktemplate_blob, &proof, Some(ts))?) {
            Ok(()) => {
                mined += 1;
                println!(
                    "  BLOCK {} ACCEPTED ({:.6} SM64 reward, {} frames = {:.1} s of play, {:.0} s to find)",
                    t.height,
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
