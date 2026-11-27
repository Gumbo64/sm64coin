//! sm64coin-automine: mines SM64Coin by playing Super Mario 64 automatically. A thin command line over
//! [`sm64coin_miner::campaign::Campaign`], which is the one mining loop (also the Android app's): fetch a block template,
//! start from the parent's end state (or power-on, by the reset policy), play on to the game's next progress point with the
//! tree search, seal the proof, sign it and submit it ONCE (one run per key: the better runs found meanwhile stay private; the
//! best one is sent `--submit-lead` seconds before it is valid, see `signed.rs`). A winning run longer than the current time
//! limit waits until the limit has grown to it, unless the tip changes; a new block restarts the attempt on a fresh template.
//!
//! The search steps run on threads of this process; `--process` runs each in an `rrtrun` process instead (crash and memory
//! isolation). The desktop app's contract: `--viz` sends the visualisation frames up stdout (`\x01VIZ\t0\tkind\tkey\tpayload`)
//! and reads the page's mailbox from stdin (JSON lines); a `progress frames=<n> fps=<n>` line every ~2 s; the log on stdout.

use anyhow::{Result, bail};
use clap::Parser;
use sm64coin_miner::campaign::{Campaign, CampaignConfig, Event, Normalise, SavedBlock, now, valid_from};
use sm64coin_miner::{rh, rpc, rrt};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    /// The desktop app's visualisation feed: the search trees are sampled and sent up on stdout as `\x01VIZ` frames, the page's
    /// requests (feed on/off, sampling bubble, node requests, hybrid commands) are read from stdin as JSON lines (rrt/vizio.rs).
    /// Without it stdin/stdout carry nothing extra.
    #[arg(long)]
    viz: bool,
    /// Run each search step in an `rrtrun` process (next to this binary) instead of on a thread of this one: a crashing or
    /// leaking step then cannot take the miner down.
    #[arg(long)]
    process: bool,
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
    /// The campaign per block: emulated frames (all threads) a castle tree / a course tree may use before the budget
    /// multiplier, a wall-clock cap of one tree (s, 0: none), checkpoints in a row before a course is given up, the course
    /// trees' stall stop in frames (0: off), more tree settings "VAR=value ..", scratch directory (also the campaign's
    /// memory across blocks and its roadmap), search threads.
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
    #[arg(long, default_value_t = 4, overrides_with = "rh_threads")]
    rh_threads: usize,
    /// Frames a run may take (0: the most any difficulty allows, 36000).
    #[arg(long, default_value_t = 0)]
    rh_frames: usize,
    /// Stop after this many blocks (0: forever).
    #[arg(long, default_value_t = 0)]
    blocks: u64,
    /// Play the fixed boot / settle input schedules of v0.4.0 (1680 frames from power-on, then wait for Mario to
    /// stand still) instead of the state-reactive macros (sm64coin_miner::macros: the first controllable frame,
    /// 36+ frames sooner from power-on). Either is just inputs in the proof. Also: SM64COIN_LEGACY_MACROS=1.
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    legacy_macros: bool,
    /// Save each sealed proof's pads here as <height>.m64 (from the block's start state, not from power-on).
    #[arg(long)]
    save_dir: Option<String>,
    /// Testing the hold policy: submit a run that waits for the limit this many seconds early, timestamped when it becomes
    /// valid (nodes accept stamps up to 120 s ahead; an honest node holds the block until its own clock lets the run fit).
    /// 0: off.
    #[arg(long, default_value_t = 0)]
    stamp_ahead: u64,
    /// Testing the hold policy deterministically: seal the first block and save it here (JSON) instead of submitting it,
    /// then exit.
    #[arg(long)]
    save_block: Option<String>,
    /// With --save-block's file: print the fixed difficulty at which its run becomes valid `--hold-in` seconds from now
    /// (restart the test node with it), then exit.
    #[arg(long)]
    pick_difficulty: Option<String>,
    /// Seconds from now for --pick-difficulty.
    #[arg(long, default_value_t = 160)]
    hold_in: u64,
    /// With --save-block's file: stamp the block at the time its run fits the daemon's current limit and submit it now
    /// (early, if that is ahead), then exit.
    #[arg(long)]
    submit_saved: Option<String>,
    /// Added to every search's seed (different runs from the same template, e.g. for tests).
    #[arg(long, default_value_t = 0)]
    salt: u64,
    /// One run per key (v0.3): the miner key signs at most ONE run per template, because a second different signed run is
    /// equivocation and honest nodes avoid the block. So the miner searches on privately (a run over the current limit waits
    /// for the limit to grow, and meanwhile fresh searches look for a shorter one) and submits the best run exactly once, this
    /// many seconds before it is valid (stamped with the second it becomes valid; nodes hold it until then). A run that is
    /// valid now is sent at once.
    #[arg(long, default_value_t = 5)]
    submit_lead: u64,
    /// Seal the run as played, without the input normaliser (sm64pow::normalise: pads that don't change the game are replaced
    /// by cheaper ones, replay-checked; seconds per block, ~12-21 % smaller proofs, which win equal-frame ties).
    #[arg(long)]
    no_normalise: bool,
    /// Use the replay-free exact canonicaliser (sm64pow::exact: ~0.1-1 s per block, ~5 % smaller proofs) instead of the
    /// replay-guided normaliser (10-60 s, ~20 % smaller).
    #[arg(long)]
    exact_normalise: bool,
    /// Search on for a shorter run while a found run waits for the limit.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    anytime: bool,
    /// The wallet's secret view key (hex) for --address: with it each template commits a fresh payout subaddress (0, k) (k
    /// from a counter kept next to --signed-file) so a proof record does not link your runs to one address; the block reward
    /// itself still goes to --address. Without it the template's payout is --address.
    #[arg(long)]
    payout_view_secret: Option<String>,
    /// Which seeds this miner already signed a run for (seed -> proof id), kept across restarts so a restart never signs a
    /// second run for the same seed.
    #[arg(long, default_value = "run/automine-signed.json")]
    signed_file: String,
}

fn main() -> Result<()> {
    let a = Args::parse();
    rh::set_legacy_macros(a.legacy_macros);
    sm64coin_miner::update::spawn_check();
    if !a.address.starts_with("SM6") {
        // Not a mainnet address (SM64...): a test network's (--testnet, --stagenet) is checked by the daemon.
        eprintln!("note: {} is not a mainnet SM64Coin address; the daemon checks it for its network", a.address);
    }
    let rom = std::fs::read(&a.rom)?;
    sm64pow::init(rom.clone())?;
    if let Some(f) = &a.pick_difficulty {
        return pick_difficulty(&a, f);
    }
    if let Some(f) = &a.submit_saved {
        return submit_saved(&a, f);
    }

    let mut cfg = CampaignConfig::new(&a.daemon, &a.address, rom, "run".into());
    cfg.threads = a.rh_threads;
    cfg.viz = a.viz;
    cfg.payout_view_secret = a.payout_view_secret.clone();
    cfg.reset = a.reset.clone();
    cfg.hub_frames = a.rrt_hub_frames;
    cfg.course_frames = a.rrt_course_frames;
    cfg.step_seconds = a.rrt_step_seconds;
    cfg.checkpoints = a.rrt_checkpoints;
    cfg.stall = a.rrt_stall;
    cfg.extra_env = a.rrt_env.clone();
    cfg.scratch = Some(a.rrt_scratch.clone().into());
    cfg.signed_file = Some(a.signed_file.clone().into());
    cfg.frame_cap = a.rh_frames;
    cfg.anytime = a.anytime;
    cfg.submit_lead = a.submit_lead;
    cfg.normalise = if a.no_normalise { Normalise::Off } else if a.exact_normalise { Normalise::Exact } else { Normalise::Replay };
    cfg.max_blocks = a.blocks;
    cfg.salt = a.salt;
    cfg.in_process = !a.process;
    cfg.rom_path = Some(a.rom.clone());
    cfg.reset_after = a.reset_after;
    cfg.stamp_ahead = a.stamp_ahead;
    cfg.save_block = a.save_block.as_ref().map(Into::into);
    cfg.save_dir = a.save_dir.as_ref().map(Into::into);
    if a.payout_view_secret.is_some() {
        println!("payout: a fresh subaddress (0, k) per template (counter {}.payout)", a.signed_file);
    }
    println!("rrt coverage:");
    for (point, how) in rrt::coverage() {
        println!("  {point}  <-  {how}");
    }
    println!("search steps: {}", if a.process { "one rrtrun process each (--process)" } else { "threads of this process" });

    let c = Arc::new(Campaign::start(cfg)?);
    let events = c.subscribe();
    if a.viz {
        let c = c.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lines().map_while(Result::ok) {
                c.mailbox(&line);
            }
        });
    }
    // The miner's speed (its "hashrate"): a machine-readable line every ~2 s, `progress frames=<simulated frames so far>
    // fps=<frames/s over the last interval>`, for the wallet GUI.
    let every = Duration::from_secs(2);
    let mut last = Instant::now();
    loop {
        match events.recv_timeout(every.saturating_sub(last.elapsed()).max(Duration::from_millis(10))) {
            Ok(Event::Log(l)) => println!("{l}"),
            Ok(Event::Viz { kind, key, payload }) => println!("{}\t0\t{kind}\t{key}\t{payload}", rrt::vizio::MARK),
            Ok(Event::Stopped(None)) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Ok(Event::Stopped(Some(e))) => bail!("{e}"),
            Ok(Event::State(_) | Event::Block(_)) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if last.elapsed() >= every {
            last = Instant::now();
            let s = c.stats();
            println!("progress frames={} fps={:.0}", s.frames, s.fps);
        }
    }
}

/// --pick-difficulty: the largest difficulty whose limit still lets the saved run in `--hold-in` seconds from now.
fn pick_difficulty(a: &Args, f: &str) -> Result<()> {
    let b: SavedBlock = serde_json::from_str(&std::fs::read_to_string(f)?)?;
    let r = sm64pow::rules();
    let at = now() + a.hold_in;
    // The limit grows with elapsed time and shrinks with difficulty.
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
    Ok(())
}

/// --submit-saved: stamps the saved block at the time its run fits the daemon's current limit and submits it.
fn submit_saved(a: &Args, f: &str) -> Result<()> {
    let mut daemon = rpc::Daemon::new(&a.daemon);
    if let Some(v) = &a.payout_view_secret {
        daemon = daemon.with_payouts(sm64coin_miner::payout::Payouts::new(&a.address, v, &a.signed_file)?);
    }
    let b: SavedBlock = serde_json::from_str(&std::fs::read_to_string(f)?)?;
    let t = daemon.template(&a.address)?;
    if t.prev_hash != b.prev_hash {
        bail!("the tip moved: the saved block is on {}, the daemon's tip is {}", b.prev_hash, t.prev_hash);
    }
    let retarget = t.sm64_retarget_difficulty.max(1);
    let ts = valid_from(retarget, b.parent_ts, b.frames, now());
    let r = sm64pow::rules();
    println!("saved block {}: {} frames; difficulty {retarget}, limit now {}; stamped {ts} = now +{} s", b.height, b.frames, r.max_frames(retarget, now().saturating_sub(b.parent_ts)), ts as i64 - now() as i64);
    let key = sm64pow::sign::Keypair::from_secret(&<[u8; 32]>::try_from(hex::decode(&b.key_secret)?.as_slice())?).ok_or_else(|| anyhow::anyhow!("the saved block has no miner key"))?;
    let mut signed = sm64coin_miner::signed::SignedStore::open(Some(&a.signed_file));
    let proof = hex::decode(&b.proof)?;
    if let Err(prev) = signed.may_sign(&b.seed_hash, &proof) {
        bail!("already signed a different run ({} frames) for this seed: not signing a second one", prev.frames);
    }
    signed.record(&b.seed_hash, &proof, b.frames)?;
    match daemon.submit(&rpc::seal_with(&b.template_blob, &b.seed_hash, &key, &proof, Some(ts))?) {
        Ok(()) => println!("submitted"),
        Err(e) => println!("submit failed: {e}"),
    }
    Ok(())
}
