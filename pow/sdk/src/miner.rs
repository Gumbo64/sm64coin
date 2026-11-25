//! The [`Bot`] trait, the daemon mining loop and offline practice.

use crate::daemon::{Daemon, seal, sign_proof};
use sm64pow::sign::Keypair;
use crate::env::{MiningEnv, Task};
use anyhow::{Result, bail};
use sm64pow::Verdict;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A mining bot. Play the run: call [`MiningEnv::step`] until
/// `env.is_success()` (the game's next progress point: a star, a Bowser key or
/// the Grand Star, from wherever the block starts), then return. Return early
/// (without success) if `stop()` becomes true (a new block arrived) or you
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
    /// One run per key (v0.3): the miner key signs at most ONE run per template (a second
    /// different signed run is equivocation, and honest nodes avoid the block). The run is
    /// kept private and submitted once, this many seconds before it is valid (stamped with
    /// the second it becomes valid; nodes hold it until then). Already valid: at once.
    pub submit_lead_secs: u64,
    /// Where to remember which seeds a run was signed for (seed -> proof id), so a
    /// restart never signs a second run for the same seed. "" keeps it in memory only.
    pub signed_file: String,
}

impl Default for MinerConfig {
    fn default() -> MinerConfig {
        MinerConfig {
            daemon_url: "http://127.0.0.1:16481".into(),
            address: String::new(),
            rom: String::new(),
            blocks: 0,
            verbose: true,
            submit_lead_secs: 5,
            signed_file: std::env::var("SM64COIN_SIGNED_FILE").unwrap_or_else(|_| "sm64coin-signed.json".into()),
        }
    }
}

/// The record of signed runs (one run per key): seed hash -> proof id and frames,
/// kept in a JSON file.
#[derive(Debug, Default)]
pub struct SignedStore {
    path: Option<std::path::PathBuf>,
    map: std::collections::BTreeMap<String, (String, u32, u64)>,
}

impl SignedStore {
    /// Loads `path` ("": memory only; a missing file: empty).
    pub fn open(path: &str) -> SignedStore {
        let path = (!path.is_empty()).then(|| std::path::PathBuf::from(path));
        let map = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        SignedStore { path, map }
    }

    /// `Err((frames))` of the run already signed for `seed_hash` if it is a different run than `proof`.
    pub fn may_sign(&self, seed_hash: &str, proof: &[u8]) -> std::result::Result<(), u32> {
        match self.map.get(seed_hash) {
            Some((id, frames, _)) if *id != hex::encode(sm64pow::proof_id(proof)) => Err(*frames),
            _ => Ok(()),
        }
    }

    /// Records `proof` for `seed_hash` and saves the file; call BEFORE signing.
    pub fn record(&mut self, seed_hash: &str, proof: &[u8], frames: u32) -> Result<()> {
        self.map.insert(seed_hash.to_string(), (hex::encode(sm64pow::proof_id(proof)), frames, now()));
        while self.map.len() > 256 {
            let oldest = self.map.iter().min_by_key(|(_, v)| v.2).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => self.map.remove(&k),
                None => break,
            };
        }
        if let Some(p) = &self.path {
            let tmp = p.with_extension("tmp");
            std::fs::write(&tmp, serde_json::to_string(&self.map)?)?;
            std::fs::rename(&tmp, p)?;
        }
        Ok(())
    }
}

/// Mines forever with `bot` (see [`run_miner_with`]).
pub fn run_miner(bot: &mut dyn Bot, daemon_url: &str, address: &str, rom: &str) -> Result<()> {
    run_miner_with(bot, &MinerConfig { daemon_url: daemon_url.into(), address: address.into(), rom: rom.into(), ..Default::default() })
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// Smallest timestamp at or after `from` at which a block mined on a parent
/// timestamped `parent_ts` may carry a run of `frames` frames.
fn valid_from(retarget: u64, parent_ts: u64, frames: u32, from: u64) -> u64 {
    let r = sm64pow::rules();
    let lo0 = from.max(parent_ts);
    if r.max_frames(retarget, lo0 - parent_ts) >= frames {
        return lo0;
    }
    let (mut lo, mut hi) = (lo0, lo0 + 1);
    while r.max_frames(retarget, hi - parent_ts) < frames {
        hi = lo0 + (hi - lo0) * 2;
        if hi - parent_ts > 1 << 40 {
            return u64::MAX;
        }
    }
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if r.max_frames(retarget, mid - parent_ts) >= frames { hi = mid } else { lo = mid }
    }
    hi
}

/// The mining loop. Per template: build the [`Task`] (the parent's end state
/// from `get_sm64_cycle`, or power-on), run the bot while a watcher thread
/// raises `stop` on a new block, check the proof locally, wait if the run is
/// longer than the current time limit (it becomes valid as the limit grows),
/// sign the proof with the template's miner key once (one run per key: the run is
/// remembered in `cfg.signed_file`), seal proof and signature into the template and submit
/// it `cfg.submit_lead_secs` before it is valid. The task allows over-budget runs
/// ([`Task::allow_over_budget`]).
pub fn run_miner_with(bot: &mut dyn Bot, cfg: &MinerConfig) -> Result<()> {
    run_miner_keyed(bot, cfg, &mut |_| Ok(Keypair::generate()))
}

/// [`run_miner_with`] with your own miner keys: `next_key(round)` is called
/// before each template request (`round` counts from 0) and must return the
/// key for that template. The default generates a fresh random key; a group
/// miner returns the secret its leader shared for this template
/// (`Keypair::from_secret`), so that any member can sign and submit. The key
/// is committed in the coinbase, so it must be fixed before the run is played.
pub fn run_miner_keyed(bot: &mut dyn Bot, cfg: &MinerConfig, next_key: &mut dyn FnMut(u64) -> Result<Keypair>) -> Result<()> {
    if !cfg.address.starts_with("SM6") {
        bail!("{} does not look like an SM64Coin address", cfg.address);
    }
    crate::init_rom(&cfg.rom)?;
    let daemon = Daemon::new(&cfg.daemon_url);
    let mut mined = 0u64;
    let mut round = 0u64;
    let mut signed = SignedStore::open(&cfg.signed_file);
    let lead = cfg.submit_lead_secs.min(sm64pow::rules::MAX_HOLD_AHEAD_SECONDS.saturating_sub(10));
    loop {
        let key = next_key(round)?;
        round += 1;
        let t = match daemon.template(&cfg.address, &key) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("daemon: {e}; retrying in 5 s");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        let task = match Task::from_template(&t, &daemon) {
            Ok(task) => task.allow_over_budget(),
            Err(e) => {
                eprintln!("cannot start the block's task: {e:#}; retrying in 5 s");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        if cfg.verbose {
            println!(
                "block {} start {:?} ({} stars {} keys), limit {} frames now, {} pokes per {}",
                t.height, task.start, task.start_progress.stars, task.start_progress.keys, t.sm64_max_frames, t.sm64_random_amount, t.sm64_window_length
            );
        }
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (stop, done) = (stop.clone(), done.clone());
            let (url, prev) = (cfg.daemon_url.clone(), t.tip.clone());
            std::thread::spawn(move || {
                let d = Daemon::new(&url);
                while !done.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(2));
                    if matches!(d.top_hash(), Ok(h) if h != prev) {
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
                println!("  no progress (stopped at frame {}); fetching a new template", env.frame());
            }
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let proof = env.proof();
        let frames = env.frame() as u32;
        match task.check(&proof)? {
            Verdict::Valid { frames } => {
                if cfg.verbose {
                    println!("  proof checked locally: {frames} frames, {} bytes, found in {:.1} s", proof.len(), t0.elapsed().as_secs_f64())
                }
            }
            Verdict::Invalid(why) => {
                eprintln!("  own proof rejected by the verifier: {why} (bug in the bot's use of the env?)");
                continue;
            }
        }
        // Over the time limit: valid once the limit has grown to the run's length.
        let retarget = task.difficulty.max(1);
        let mut ts = valid_from(retarget, t.sm64_parent_timestamp, frames, now());
        if ts > now() {
            let asked = daemon.template_valid_at(&cfg.address, &key, frames).map(|x| x.sm64_valid_at).unwrap_or(0);
            ts = ts.max(asked);
            if cfg.verbose {
                println!("  {frames} frames is over the current limit; valid at {ts} (in {} s)", ts.saturating_sub(now()));
            }
            let mut abandoned = false;
            while now() + lead < ts {
                std::thread::sleep(Duration::from_secs(1));
                if now() % 3 == 0 && matches!(daemon.top_hash(), Ok(h) if h != t.tip) {
                    abandoned = true;
                    break;
                }
            }
            if abandoned {
                println!("  new block on the network: abandoning the waiting run");
                continue;
            }
            ts = ts.max(now());
        } else {
            ts = now();
        }
        // One run per key: never sign a second, different run for this seed.
        if let Err(prev) = signed.may_sign(&t.sm64_seed_hash, &proof) {
            println!("  already signed a different run ({prev} frames) for this seed: not signing a second one; waiting for a new block");
            while matches!(daemon.top_hash(), Ok(h) if h == t.tip) {
                std::thread::sleep(Duration::from_secs(2));
            }
            continue;
        }
        signed.record(&t.sm64_seed_hash, &proof, frames)?;
        let sig = sign_proof(&key, &t, &proof)?;
        match daemon.submit(&seal(&t.blocktemplate_blob, &proof, &sig, Some(ts))?) {
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
    let verdict = if success { Some(task.check(&proof)?) } else { None };
    Ok(PracticeReport { seed, difficulty, frames: env.frame(), max_frames: env.max_frames(), success, verdict, seconds, proof })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_run_per_seed_and_it_survives_a_restart() {
        let f = std::env::temp_dir().join(format!("sdk-signed-{}.json", std::process::id()));
        let f = f.to_str().unwrap();
        let (a, b) = (vec![1u8, 2, 3], vec![9u8, 9, 9]);
        let mut s = SignedStore::open(f);
        assert!(s.may_sign("seed", &a).is_ok());
        s.record("seed", &a, 100).unwrap();
        assert!(s.may_sign("seed", &a).is_ok(), "the same run is the same block");
        assert_eq!(s.may_sign("seed", &b), Err(100), "a second, different run is equivocation");
        assert_eq!(SignedStore::open(f).may_sign("seed", &b), Err(100), "remembered after a restart");
        assert!(SignedStore::open("").may_sign("seed", &b).is_ok(), "no file: memory only");
        std::fs::remove_file(f).ok();
    }

    #[test]
    fn default_lead_is_five_seconds() {
        assert_eq!(MinerConfig::default().submit_lead_secs, 5);
    }
}
