//! The [`Bot`] trait, the daemon mining loop and offline practice.

use crate::daemon::{Daemon, seal};
use crate::env::{MiningEnv, Task};
use anyhow::{Result, bail};
use sm64pow::Verdict;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A mining bot. Play the run: call [`MiningEnv::step`] until
/// `env.is_success()`, then return. Return early (without success) if
/// `stop()` becomes true (a new block arrived, the task got easier) or you
/// give up; the miner then fetches a fresh template and calls you again.
///
/// A bot may be called many times (once per template); reset your own state in
/// [`Bot::play`].
pub trait Bot {
    fn play(&mut self, env: &mut MiningEnv, stop: &dyn Fn() -> bool) -> Result<()>;
}

#[derive(Clone, Debug)]
pub struct MinerConfig {
    pub daemon_url: String,
    pub address: String,
    /// Path to the US ROM ("" : `$SM64COIN_ROM` or `./baserom.us.z64`).
    pub rom: String,
    /// Stop after this many accepted blocks (0: forever).
    pub blocks: u64,
    /// Print progress.
    pub verbose: bool,
}

/// Mines forever with `bot`. Per template: build the task, run the bot while a
/// watcher thread raises `stop` on a new block, verify the proof locally, seal
/// it into the template and submit.
pub fn run_miner(bot: &mut dyn Bot, daemon_url: &str, address: &str, rom: &str) -> Result<()> {
    run_miner_with(bot, &MinerConfig { daemon_url: daemon_url.into(), address: address.into(), rom: rom.into(), blocks: 0, verbose: true })
}

pub fn run_miner_with(bot: &mut dyn Bot, cfg: &MinerConfig) -> Result<()> {
    if !cfg.address.starts_with("SM6") {
        bail!("{} does not look like an SM64Coin address", cfg.address);
    }
    crate::init_rom(&cfg.rom)?;
    let daemon = Daemon::new(&cfg.daemon_url);
    let mut mined = 0u64;
    loop {
        let t = match daemon.template(&cfg.address) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("daemon: {e}; retrying in 5 s");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        let task = Task::from_template(&t)?;
        if cfg.verbose {
            println!(
                "block {} difficulty {} limit {} frames, {} pokes per {}",
                t.height, t.difficulty, t.sm64_max_frames, t.sm64_random_amount, t.sm64_window_length
            );
        }
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (stop, done) = (stop.clone(), done.clone());
            let (url, prev, easier) = (cfg.daemon_url.clone(), t.prev_hash.clone(), t.sm64_easier_at);
            std::thread::spawn(move || {
                let d = Daemon::new(&url);
                while !done.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(2));
                    if matches!(d.top_hash(), Ok(h) if h != prev) {
                        stop.store(true, Ordering::Relaxed);
                        return;
                    }
                    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                    if easier != 0 && now >= easier {
                        stop.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            })
        };
        let t0 = Instant::now();
        let mut env = MiningEnv::new(&task)?;
        let res = {
            let stop = stop.clone();
            bot.play(&mut env, &move || stop.load(Ordering::Relaxed))
        };
        done.store(true, Ordering::Relaxed);
        let _ = watcher.join();
        if let Err(e) = res {
            eprintln!("bot error: {e:#}");
        }
        if !env.is_success() {
            if cfg.verbose {
                println!("  no star (stopped at frame {}); fetching a new template", env.frame());
            }
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let proof = env.proof();
        match sm64pow::verify(&task.seed, task.difficulty, &proof)? {
            Verdict::Valid { frames } => {
                if cfg.verbose {
                    println!("  proof verified locally: {frames} frames, {} bytes, found in {:.1} s", proof.len(), t0.elapsed().as_secs_f64())
                }
            }
            Verdict::Invalid(why) => {
                eprintln!("  own proof rejected by the verifier: {why} (bug in the bot's use of the env?)");
                continue;
            }
        }
        match daemon.submit(&seal(&t.blocktemplate_blob, &proof)?) {
            Ok(()) => {
                mined += 1;
                println!("  BLOCK {} ACCEPTED ({:.6} SM64)", t.height, t.expected_reward as f64 / 1e12);
                if cfg.blocks != 0 && mined >= cfg.blocks {
                    return Ok(());
                }
            }
            Err(e) => println!("  block rejected: {e}"),
        }
    }
}

/// The outcome of one offline [`practice`] run.
#[derive(Debug, Clone)]
pub struct PracticeReport {
    pub seed: u64,
    pub difficulty: u64,
    pub frames: usize,
    pub max_frames: usize,
    pub success: bool,
    /// The consensus verifier's verdict on the proof (only checked on success).
    pub verdict: Option<Verdict>,
    pub seconds: f64,
    pub proof: Vec<u8>,
}

impl PracticeReport {
    pub fn fps(&self) -> f64 {
        self.frames as f64 / self.seconds.max(1e-9)
    }
}

/// Offline practice: runs `bot` on a local task (no daemon), and when it
/// reaches the success condition verifies the proof with the consensus
/// verifier. `stop` ends the run early.
pub fn practice(bot: &mut dyn Bot, seed: u64, difficulty: u64, stop: &dyn Fn() -> bool) -> Result<PracticeReport> {
    let task = Task::practice(seed, difficulty);
    let mut env = MiningEnv::new(&task)?;
    let t0 = Instant::now();
    bot.play(&mut env, stop)?;
    let seconds = t0.elapsed().as_secs_f64();
    let (success, proof) = (env.is_success(), env.proof());
    let verdict = if success { Some(sm64pow::verify(&task.seed, difficulty, &proof)?) } else { None };
    Ok(PracticeReport { seed, difficulty, frames: env.frame(), max_frames: env.max_frames(), success, verdict, seconds, proof })
}
