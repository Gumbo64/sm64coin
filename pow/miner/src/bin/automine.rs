//! sm64coin-automine: mines SM64Coin by playing Super Mario 64 automatically.
//!
//! Loop: fetch a block template from the daemon, build the block's start
//! state (power-on, or the parent's end state: the current cycle's played pads
//! replayed unperturbed, cached per tip), play on from there to the game's
//! next progress point (rh.rs: a route library keyed by the stars already
//! collected), seal the proof with `seal_proof_at`, sign it and submit it ONCE
//! (one run per key: the better runs found meanwhile stay private; the best one is sent
//! `--submit-lead` seconds before it is valid, see `signed.rs`). A winning run longer than the current time limit waits until the
//! limit has grown to it (continuous decay, `sm64_valid_at`) unless the tip
//! changes. A new block on the network restarts the attempt on a fresh template.

use anyhow::{Result, bail};
use clap::Parser;
use sm64coin_miner::{beam, reset, rh, routes::Library, rpc, rrt};
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
    /// Reset cycle (power-on start) policy, when the chain allows it (template sm64_start "either"):
    /// `never` (only when the chain forces it), `when-stuck` (search the parent's end state first; take
    /// power-on if that finds no route / gives up / times out / the parent has 120 stars; and restart when
    /// a "parent" template's sm64_power_on_at passes), `always-when-allowed` (power-on as soon as allowed).
    #[arg(long, default_value = "when-stuck")]
    reset: String,
    /// when-stuck: also give up the parent-start search after this many seconds on an "either" template
    /// and take power-on (0: only when the search itself gives up).
    #[arg(long, default_value_t = 0)]
    reset_after: u64,
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
    /// One run per key (v0.3): the miner key signs at most ONE run per template, because
    /// a second different signed run is equivocation and honest nodes avoid the block. So
    /// the miner searches on privately (a run over the current limit waits for the limit to
    /// grow, and meanwhile fresh searches look for a shorter one) and submits the best run
    /// exactly once, this many seconds before it is valid (stamped with the second it
    /// becomes valid; nodes hold it until then). A run that is valid now is sent at once.
    #[arg(long, default_value_t = 5)]
    submit_lead: u64,
    /// Seal the run as played, without the input normaliser (sm64pow::normalise: pads that don't
    /// change the game are replaced by cheaper ones, replay-checked; seconds per block, ~12-21 %
    /// smaller proofs, which win equal-frame ties).
    #[arg(long)]
    no_normalise: bool,
    /// Search on for a shorter run while a found run waits for the limit (rrt only).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    anytime: bool,
    /// The wallet's secret view key (hex) for --address: with it each template commits a fresh payout
    /// subaddress (0, k) (k from a counter kept next to --signed-file) so a proof record does not link
    /// your runs to one address; the block reward itself still goes to --address. Without it the
    /// template's payout is --address.
    #[arg(long)]
    payout_view_secret: Option<String>,
    /// Which seeds this miner already signed a run for (seed -> proof id), kept across
    /// restarts so a restart never signs a second run for the same seed.
    #[arg(long, default_value = "run/automine-signed.json")]
    signed_file: String,
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
    /// v0.3: the template's seed and miner key (secret, hex), to sign at submit.
    #[serde(default)]
    seed_hash: String,
    #[serde(default)]
    key_secret: String,
    /// The key's signature of this one run (hex, 64 bytes): the seed excludes the timestamp, so a
    /// restamped block keeps it (the web test tooling can't sign in node).
    #[serde(default)]
    sig: String,
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
    sm64coin_miner::update::spawn_check();
    if !a.address.starts_with("SM6") {
        // Not a mainnet address (SM64...): a test network's (--testnet, --stagenet) is checked by the daemon.
        eprintln!("note: {} is not a mainnet SM64Coin address; the daemon checks it for its network", a.address);
    }
    sm64pow::init(std::fs::read(&a.rom)?)?;
    let mut daemon = rpc::Daemon::new(&a.daemon);
    if let Some(v) = &a.payout_view_secret {
        daemon = daemon.with_payouts(sm64coin_miner::payout::Payouts::new(&a.address, v, &a.signed_file)?);
        println!("payout: a fresh subaddress (0, k) per template (counter {}.payout)", a.signed_file);
    }
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
        let key = sm64pow::sign::Keypair::from_secret(&<[u8; 32]>::try_from(hex::decode(&b.key_secret)?.as_slice())?).ok_or_else(|| anyhow::anyhow!("the saved block has no miner key"))?;
        let mut signed = sm64coin_miner::signed::SignedStore::open(Some(&a.signed_file));
        let proof = hex::decode(&b.proof)?;
        if let Err(prev) = signed.may_sign(&b.seed_hash, &proof) {
            bail!("already signed a different run ({} frames) for this seed: not signing a second one", prev.frames);
        }
        signed.record(&b.seed_hash, &proof, b.frames)?;
        match daemon.submit(&rpc::seal_with(&b.template_blob, &b.seed_hash, &key, &hex::decode(&b.proof)?, Some(ts))?) {
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
    // The miner's speed (its "hashrate"): a machine-readable line every ~2 s, `progress frames=<simulated frames so far>
    // fps=<frames/s over the last interval>`, for the wallet GUI (the rrt controller's search steps; 0 otherwise).
    std::thread::spawn(|| {
        let (mut last, mut at) = (0u64, std::time::Instant::now());
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let n = sm64coin_miner::rrt::SIM_FRAMES.load(std::sync::atomic::Ordering::Relaxed);
            let dt = at.elapsed().as_secs_f64();
            println!("progress frames={n} fps={:.0}", (n - last) as f64 / dt.max(1e-3));
            (last, at) = (n, std::time::Instant::now());
        }
    });
    let mut mined = 0u64;
    let reset_mode: reset::ResetMode = a.reset.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    println!("reset cycle policy: {:?} (--reset), parent-search time limit {} s", reset_mode, a.reset_after);
    let mut attempt = 0u64;
    let mut signed = sm64coin_miner::signed::SignedStore::open(Some(&a.signed_file));
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
        let seed = match sm64pow::Seed::from_hex(&t.sm64_seed_hash) {
            Ok(s) => s,
            Err(e) => {
                println!("daemon: bad seed in the template ({e}); retrying in 5 s");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
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
            t.prev_hash.get(..12).unwrap_or(&t.prev_hash),
            t.sm64_retarget_difficulty,
            t.sm64_max_frames,
            t.sm64_start,
            t.sm64_parent_stars,
            t.sm64_parent_keys,
            t.sm64_seed_hash.get(..8).unwrap_or(&t.sm64_seed_hash)
        );

        // Where the run starts: the parent's end state, or power-on.
        let (pick, why_pick) = reset::initial(reset_mode, &t.sm64_start, t.sm64_parent_stars);
        let mut start = if pick == reset::Pick::PowerOn { Start::PowerOn } else { Start::Parent };
        println!("  reset policy {:?}: start {:?} ({why_pick}){}", reset_mode, start, if t.sm64_start == "parent" { format!("; power-on allowed in {} s", reset::seconds_until_allowed(&t.sm64_start, t.sm64_power_on_at, now())) } else { String::new() });
        let mut parent_state: Option<SaveState> = None;
        if start == Start::Parent {
            if parent_cache.as_ref().is_none_or(|c| c.tip != t.prev_hash) {
                let from_node = daemon.end_state(&t.prev_hash).and_then(|s| rpc::state_progress(&s).map(|p| (s, p)));
                let built = match from_node {
                    Ok((state, p)) => {
                        println!("  parent state: from the node (get_sm64_state) -> {} stars {} keys", p.stars, p.keys);
                        Ok((state, p))
                    }
                    Err(e0) => daemon.cycle(Some(&t.prev_hash)).and_then(|c| rpc::cycle_end_state(&c).map(|(s, p)| (c, s, p))).map(|(c, state, p)| {
                        println!(
                            "  parent state: cycle replay ({e0:#}): {} blocks, {} frames replayed -> {} stars {} keys",
                            c.blocks.len(),
                            c.pads.len(),
                            p.stars,
                            p.keys
                        );
                        (state, p)
                    }),
                };
                match built {
                    Ok((state, _p)) => {
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
        let pstop = Arc::new(AtomicBool::new(false)); // the parent-start search only (--reset-after)
        let done = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (stop, done) = (stop.clone(), done.clone());
            let (url, prev) = (a.daemon.clone(), t.tip.clone());
            // A parent-only template becomes "either" at sm64_power_on_at.
            let power_on_at = reset::restart_at_power_on_time(reset_mode, &t.sm64_start, t.sm64_power_on_at).unwrap_or(0);
            let deadline = if reset_mode == reset::ResetMode::WhenStuck && t.sm64_start == "either" && start == Start::Parent && a.reset_after > 0 { now() + a.reset_after } else { 0 };
            let pstop = pstop.clone();
            std::thread::spawn(move || {
                let d = rpc::Daemon::new(&url);
                while !done.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(2));
                    if power_on_at != 0 && now() >= power_on_at {
                        println!("  the stall time has passed: restarting to allow the power-on start");
                        stop.store(true, Ordering::Relaxed);
                        return;
                    }
                    if deadline != 0 && now() >= deadline {
                        println!("  parent-start search used its --reset-after time");
                        pstop.store(true, Ordering::Relaxed);
                    }
                    if let Ok(h) = d.top_hash() {
                        if h != prev {
                            println!("  new block {}.. on the network: restarting", &h[..12]);
                            stop.store(true, Ordering::Relaxed);
                            pstop.store(true, Ordering::Relaxed);
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
            let first_stop = if start == Start::Parent { pstop.clone() } else { stop.clone() };
            let mut r = if use_rrt { survive(rrt::mine(start, parent_state.as_ref(), &seed, &params, &ro, &first_stop, |m| println!("  {m}"))) } else { survive(rh::mine(start, parent_state.as_ref(), &seed, &params, &o, &first_stop, |m| println!("  {m}"))) };
            let fallback = match &r {
                _ if start == Start::PowerOn => None,
                rh::Attempt::NoRoute(why) => Some(format!("no route from the parent's end state: {why}")),
                rh::Attempt::Gave(why) if pstop.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) => Some(format!("parent-start search timed out ({why})")),
                rh::Attempt::Gave(why) if t.sm64_start == "either" && !stop.load(Ordering::Relaxed) => Some(format!("the parent's end state didn't give progress quickly ({why})")),
                _ => None,
            };
            if let Some(why) = fallback {
                println!("  {why}");
                if reset::after_failure(reset_mode, &t.sm64_start) {
                    println!("  reset policy: taking the power-on start (reset cycle)");
                    start = Start::PowerOn;
                    parent_state = None;
                    r = if use_rrt { survive(rrt::mine(start, None, &seed, &params, &ro, &stop, |m| println!("  {m}"))) } else { survive(rh::mine(start, None, &seed, &params, &o, &stop, |m| println!("  {m}"))) };
                }
            }
            match r {
                rh::Attempt::Found(m) => result = Some(m),
                rh::Attempt::Gave(why) => println!("  no progress: {why}"),
                rh::Attempt::NoRoute(why) => no_route = Some(why),
            }
        } else if start == Start::PowerOn || t.sm64_parent_stars + t.sm64_parent_keys == 0 || reset::after_failure(reset_mode, &t.sm64_start) {
            if start == Start::Parent && t.sm64_parent_stars + t.sm64_parent_keys > 0 {
                println!("  reset policy: the beam controller can't continue from the parent: taking the power-on start (reset cycle)");
            }
            parent_state = None;
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
                    if daemon.top_hash().map(|h| h != t.tip).unwrap_or(false) {
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

        let proof = if a.save_block.is_some() { normalised(&a, start, parent_state.as_ref(), &seed, &params, &m.pads, proof) } else { proof };
        if let Some(f) = &a.save_block {
            let b = SavedBlock { template_blob: t.blocktemplate_blob.clone(), seed_hash: t.sm64_seed_hash.clone(), key_secret: t.key.as_ref().map(|k| hex::encode(k.secret)).unwrap_or_default(), sig: match &t.key { Some(k) => hex::encode(sm64pow::mining::sign_proof(k, &seed.0, &proof)?), None => String::new() }, proof: hex::encode(&proof), frames, parent_ts: t.sm64_parent_timestamp, prev_hash: t.prev_hash.clone(), height: t.height };
            std::fs::write(f, serde_json::to_string(&b)?)?;
            println!("  saved block {} ({frames} frames) to {f}, not submitted", t.height);
            return Ok(());
        }
        // Anytime mining: over the limit now, so search again (fresh trees, the guide from the
        // best run) until shortly before the best run is valid; keep the shortest.
        let (m, proof, frames) = if a.controller == "rrt" && a.anytime && a.stamp_ahead == 0 {
            match improve(&a, &daemon, &t, start, parent_state.as_ref(), &seed, &params, attempt, m, proof)? {
                Some(x) => x,
                None => continue, // a new block landed: the run is void
            }
        } else {
            (m, proof, frames)
        };
        let proof = if a.save_block.is_some() { proof } else { normalised(&a, start, parent_state.as_ref(), &seed, &params, &m.pads, proof) };
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
            let ahead = if a.stamp_ahead > 0 {
                a.stamp_ahead
            } else {
                a.submit_lead.min(sm64pow::rules::RPC_SUBMIT_AHEAD_SECONDS.saturating_sub(5))
            };
            while now() + ahead < ts {
                std::thread::sleep(Duration::from_secs(1));
                if now() % 3 == 0 && daemon.top_hash().map(|h| h != t.tip).unwrap_or(false) {
                    println!("  new block on the network: abandoning the waiting run");
                    abandoned = true;
                    break;
                }
            }
            if abandoned {
                // v0.4 stage B: a run that lost its height is still a proof for it.
                publish_proof(&daemon, &mut signed, &t, &proof, frames, now());
                continue;
            }
            ts = ts.max(now());
        } else {
            ts = now();
        }
        // One run per key: never sign a second, different run for this seed.
        if let Err(prev) = signed.may_sign(&t.sm64_seed_hash, &proof) {
            println!("  already signed a different run ({} frames) for seed {}..: not signing a second one (equivocation); waiting for a new block", prev.frames, t.sm64_seed_hash.get(..8).unwrap_or(&t.sm64_seed_hash));
            while daemon.top_hash().map(|h| h == t.tip).unwrap_or(true) {
                std::thread::sleep(Duration::from_secs(2));
            }
            continue;
        }
        signed.record(&t.sm64_seed_hash, &proof, frames)?;
        let sent = daemon.submit(&rpc::seal_signed(&t, &proof, Some(ts))?);
        match sent {
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
            Err(e) => {
                println!("  block rejected: {e}");
                // v0.4 stage B: the same signed run (no second signature), offered as a proof.
                match daemon.submit_proof(&rpc::seal_signed(&t, &proof, Some(ts))?) {
                    Ok((v, why)) => println!("  offered as a proof for height {}: {}{}", t.height, proof_verdict(v), if why.is_empty() { String::new() } else { format!(" ({why})") }),
                    Err(e) => println!("  offering it as a proof failed: {e}"),
                }
            }
        }
    }
}

fn proof_verdict(v: i64) -> &'static str {
    match v {
        1 => "in the pool",
        0 => "ignored (known, not for the node's tip height, outside the window, or pool full)",
        -2 => "the node lacks the parent state",
        -3 => "bad encoding",
        _ => "refused",
    }
}

/// v0.4 stage B: signs (once per seed: one run per key) and offers a run that can't win as a
/// proof record for its height.
fn publish_proof(daemon: &rpc::Daemon, signed: &mut sm64coin_miner::signed::SignedStore, t: &rpc::Template, proof: &[u8], frames: u32, ts: u64) {
    if signed.may_sign(&t.sm64_seed_hash, proof).is_err() || signed.record(&t.sm64_seed_hash, proof, frames).is_err() {
        return;
    }
    match rpc::seal_signed(t, proof, Some(ts)).and_then(|b| daemon.submit_proof(&b)) {
        Ok((v, why)) => println!("  published as a proof for height {}: {}{}", t.height, proof_verdict(v), if why.is_empty() { String::new() } else { format!(" ({why})") }),
        Err(e) => println!("  publishing the run as a proof failed: {e}"),
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
/// A failed attempt (e.g. a start that never settled) must not end the miner:
/// log it and give up on this template; the loop takes the next one.
fn survive(r: Result<rh::Attempt>) -> rh::Attempt {
    r.unwrap_or_else(|e| {
        println!("  attempt failed: {e:#}");
        rh::Attempt::Gave(format!("error: {e:#}"))
    })
}

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
/// searches from the same start until `submit_lead` s before the best run is valid. A shorter
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
    let ahead = a.submit_lead.min(sm64pow::rules::RPC_SUBMIT_AHEAD_SECONDS.saturating_sub(5));
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
            let (url, prev) = (a.daemon.clone(), t.tip.clone());
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

/// The sealed `proof` of the run `pads`, or a smaller one of the same run after the input normaliser
/// (sm64pow::normalise: every candidate replay-checked; the original on any doubt). Off with --no-normalise.
fn normalised(a: &Args, start: Start, parent: Option<&SaveState>, seed: &sm64pow::Seed, params: &sm64pow::Params, pads: &[sm64pow::sm64_binds::GamePad], proof: Vec<u8>) -> Vec<u8> {
    if a.no_normalise {
        return proof;
    }
    let t0 = std::time::Instant::now();
    match sm64pow::normalise::seal_proof_normalised_report(start, parent, seed, params, pads) {
        Ok((better, rep)) if better.len() < proof.len() => {
            println!("  normalised: {} -> {} bytes ({:.1} %; conservative {:?}, aggressive {:?}) in {:.1} s", proof.len(), better.len(), 100.0 * better.len() as f64 / proof.len() as f64, rep.tried[0], rep.tried[1], t0.elapsed().as_secs_f64());
            better
        }
        Ok(_) => {
            println!("  normalised: no smaller proof ({} bytes) in {:.1} s", proof.len(), t0.elapsed().as_secs_f64());
            proof
        }
        Err(e) => {
            println!("  normaliser failed ({e:#}); sealing the run as played");
            proof
        }
    }
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
