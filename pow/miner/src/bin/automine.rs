//! sm64coin-automine: mines SM64Coin by playing Super Mario 64 automatically.
//!
//! Loop: fetch a block template from the daemon, play the run its seed and
//! difficulty ask for (following a reference route through the controller
//! perturbation, see tracker.rs), check the proof with the consensus verifier,
//! put it in the block and submit. A new block on the network, or the
//! difficulty decaying, restarts the attempt on a fresh template.

use anyhow::{Result, bail};
use clap::Parser;
use sm64coin_miner::{beam, rh, routes::Library, rpc};
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
    /// Reference route(s) (.m64) that meet the success condition unperturbed.
    /// The first that works is used; others are fallbacks.
    /// (The chain's own blocks are added too, see --chain-routes; the bomb-boost
    /// routes chomp-*.m64 rarely survive the perturbation, so they're opt-in.)
    #[arg(long, default_values_t = ["pow/miner/routes/block1-human.m64".to_string()])]
    route: Vec<String>,
    /// The controller: `rh` (the rolling-horizon GA over the ETA objectives of
    /// the Chomp star route, rh.rs) or `beam` (the old reference-following beam search).
    #[arg(long, default_value = "rh")]
    controller: String,
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
    /// Also learn routes from the chain: every block's winning run (default on).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    chain_routes: bool,
    /// Where to keep each route's win/attempt record.
    #[arg(long, default_value = "run/automine-routes.json")]
    route_stats: String,
    /// Routes tried per block template (best-ranked first).
    #[arg(long, default_value_t = 3)]
    routes_per_template: usize,
    /// Save each sealed proof's pads here as <height>.m64.
    #[arg(long)]
    save_dir: Option<String>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn main() -> Result<()> {
    let a = Args::parse();
    if !a.address.starts_with("SM6") {
        bail!("{} does not look like an SM64Coin address", a.address);
    }
    sm64pow::init(std::fs::read(&a.rom)?)?;
    let daemon = rpc::Daemon::new(&a.daemon);
    let mut lib = Library::new(Some(a.route_stats.clone()));
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
    if a.controller == "beam" && lib.routes.is_empty() {
        bail!("no usable reference route");
    }
    let mut mined = 0u64;
    let mut attempt = 0u64;
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
        let params = sm64pow::params(t.difficulty);
        let seed = sm64pow::Seed::from_hex(&t.sm64_seed_hash)?;
        if params.max_frames != t.sm64_max_frames
            || params.rng.random_amount != t.sm64_random_amount
            || params.reseed_frames != t.sm64_reseed_frames
        {
            bail!(
                "this miner's rules ({}, {} frames) differ from the daemon's ({}, {} frames): rebuild one of them",
                sm64pow::rules().name(),
                params.max_frames,
                t.sm64_rules,
                t.sm64_max_frames
            );
        }
        println!(
            "\nblock {} on {}..  difficulty {} (retarget {})  limit {} frames  {} pokes/{}  seed {}",
            t.height,
            &t.prev_hash[..12],
            t.difficulty,
            t.sm64_retarget_difficulty,
            t.sm64_max_frames,
            t.sm64_random_amount,
            t.sm64_window_length,
            t.sm64_seed
        );

        // Watch for a new block (or an easier task) while playing.
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (stop, done) = (stop.clone(), done.clone());
            let (url, prev, easier) = (a.daemon.clone(), t.prev_hash.clone(), t.sm64_easier_at);
            std::thread::spawn(move || {
                let d = rpc::Daemon::new(&url);
                while !done.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(2));
                    match d.top_hash() {
                        Ok(h) if h != prev => {
                            println!("  new block {}.. on the network: restarting", &h[..12]);
                            stop.store(true, Ordering::Relaxed);
                            return;
                        }
                        _ => {}
                    }
                    if easier != 0 && now() >= easier {
                        println!("  the difficulty has decayed: restarting on an easier task");
                        stop.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            })
        };

        if a.chain_routes {
            if let Ok(n) = lib.harvest(&daemon) {
                if n > 0 {
                    println!("  {n} new route(s) from the chain");
                }
            }
        }
        println!("  routes: {}", lib.summary());
        let mut result = None;
        if a.controller == "rh" {
            let o = rh::RhOptions { pop: a.rh_pop, horizon: a.rh_horizon, commit: a.rh_commit, threads: a.rh_threads, seed: attempt, ..Default::default() };
            result = rh::mine(&seed, &params, &o, &stop, |m| println!("  {m}"))?;
        }
        for (k, i) in lib.ranked().into_iter().take(if a.controller == "beam" { a.routes_per_template.max(1) } else { 0 }).enumerate() {
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
        done.store(true, Ordering::Relaxed);
        let _ = watcher.join();
        let Some(m) = result else { continue };

        // The block stores only the non-poked frames' inputs (pokes are
        // regenerated from the seed).
        let proof = sm64pow::seal_proof(&seed, &params, &m.pads)?;
        match sm64pow::verify(&seed, t.difficulty, &proof)? {
            sm64pow::Verdict::Valid { frames } => println!("  proof verified locally: {frames} frames, {} bytes", proof.len()),
            sm64pow::Verdict::Invalid(why) => {
                println!("  BUG: own proof rejected: {why}");
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
        match daemon.submit(&rpc::seal(&t.blocktemplate_blob, &proof)?) {
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
