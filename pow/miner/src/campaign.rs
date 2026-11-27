//! The auto-miner as a library: [`Campaign`] runs the RRT campaign (the one mining loop: `automine` is a thin command line
//! over it) on threads of the calling process, with no child process, no `rrtrun` binary and no file outside `data_dir`
//! (unless [`CampaignConfig::in_process`] is turned off: then each search step runs in an `rrtrun` process, for crash and
//! memory isolation). It is what the Android app calls (Android cannot spawn sidecar binaries) and what the desktop app
//! spawns as `automine`.
//!
//! ```no_run
//! use sm64coin_miner::campaign::{Campaign, CampaignConfig, Event};
//! let rom = std::fs::read("baserom.us.z64").unwrap();
//! let mut cfg = CampaignConfig::new("http://127.0.0.1:16481", "SM64...", rom, "/data/user/0/app/files/miner".into());
//! cfg.threads = 4;
//! cfg.viz = true;
//! let c = Campaign::start(cfg).unwrap();
//! let events = c.subscribe();              // log lines, state changes, viz frames, blocks
//! c.mailbox(r#"{"viz_on": true}"#);        // the page's feed switch / sampling bubble / node requests
//! while let Ok(e) = events.recv() {
//!     match e {
//!         Event::Log(l) => println!("{l}"),
//!         Event::Stopped(_) => break,
//!         _ => {}
//!     }
//!     if c.stats().blocks_sent > 0 { c.stop(); }
//! }
//! c.join();
//! ```
//!
//! What a campaign does: fetch a block template from the daemon, start from the
//! parent's end state (or power-on, by the reset policy), play on to the next progress point with the tree search (steps
//! run in-process, [`crate::rrt::child::run_step_inproc`]), seal the proof, search on for a shorter run while the run waits
//! for the time limit (anytime), sign once per seed (`data_dir/signed.json`: never two different runs for a seed) and submit.
//! Mining validity never depends on the search: the proof is sealed by the shadow replay (`sm64pow::seal_proof_at`).
//!
//! A campaign owns its viz mailbox and frame counter (`rrt::vizio::Hub`), so several can run in one process (the search's
//! "resume this block" memory is the one process-wide piece: it is keyed by the block's seed, so a neighbour costs a resume at
//! most). The search engine's own console output (`println!`) is not captured; the events carry the miner's log.
use crate::reset::{self, ResetMode};
use crate::rh::Attempt;
use crate::rpc;
use crate::rrt::{self, RrtOptions, vizio::Hub};
use crate::rh::Mined;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sm64pow::proof::Start;
use sm64pow::sm64_binds::SaveState;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How the sealed run is shrunk before it is signed (smaller proofs win equal-frame ties).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Normalise {
    /// Seal the run as played.
    Off,
    /// The replay-free exact canonicaliser (about 0.1-1 s per block, about 5 % smaller). The default: cheap on a phone.
    Exact,
    /// The replay-guided normaliser (10-60 s on a desktop, about 20 % smaller).
    Replay,
}

#[derive(Clone)]
pub struct CampaignConfig {
    /// The daemon's RPC URL (the template source), e.g. `http://127.0.0.1:16481`.
    pub daemon_url: String,
    /// The address the block reward is paid to (`SM64...`).
    pub address: String,
    /// The US ROM's bytes (the app keeps the file in its own storage; nothing is read from disk here).
    pub rom: Vec<u8>,
    /// The only directory the campaign writes: its scratch (`rrt-scratch/`), the signed-run store (`signed.json`).
    pub data_dir: PathBuf,
    /// Search threads (the rayon pool of each step).
    pub threads: usize,
    /// Sample the search trees for a viewer: [`Event::Viz`] frames (the page's feed; see rrt/vizio.rs for the kinds).
    pub viz: bool,
    /// The wallet's secret view key (hex): each template then pays a fresh subaddress (see `payout.rs`).
    pub payout_view_secret: Option<String>,
    /// Reset-cycle policy: `never`, `when-stuck` (default) or `always-when-allowed`.
    pub reset: String,
    /// Emulated frames (all threads) a castle / a course tree may use (the campaign's `--rrt-hub-frames` / `--rrt-course-frames`).
    pub hub_frames: u64,
    pub course_frames: u64,
    /// Search on for a shorter run while a found run waits for the limit.
    pub anytime: bool,
    /// Seconds before a waiting run is valid that it is submitted (one run per key: see automine's `--submit-lead`).
    pub submit_lead: u64,
    pub normalise: Normalise,
    /// Stop after this many accepted blocks (0: forever).
    pub max_blocks: u64,
    /// Added to every search's seed (different runs from one template, tests).
    pub salt: u64,
    /// Run each search step on a thread of this process (default), or in an `rrtrun` process per step (`child`, else next to
    /// the running executable; needs `rom_path`): a crashing or leaking step then cannot take the miner down.
    pub in_process: bool,
    pub child: Option<PathBuf>,
    /// The ROM's path, for the `rrtrun` steps (they load it themselves).
    pub rom_path: Option<String>,
    /// Where the step files, the campaign's memory across blocks and its roadmap live (default `data_dir/rrt-scratch`), and
    /// the signed-run store (default `data_dir/signed.json`).
    pub scratch: Option<PathBuf>,
    pub signed_file: Option<PathBuf>,
    /// Wall-clock cap of one tree in seconds (0: none), checkpoints in a row before a course is given up, the course
    /// trees' stall stop in frames (0: off), more tree settings "VAR=value ..", frames a run may take (0: the most any
    /// difficulty allows).
    pub step_seconds: f64,
    pub checkpoints: usize,
    pub stall: u64,
    pub extra_env: String,
    pub frame_cap: usize,
    /// `when-stuck`: also give up the parent-start search after this many seconds on an "either" template and take
    /// power-on (0: only when the search itself gives up).
    pub reset_after: u64,
    /// Testing the hold policy: submit a waiting run this many seconds early, stamped when it becomes valid (0: off; also
    /// turns the anytime search off).
    pub stamp_ahead: u64,
    /// Testing: seal the first block, save it here as JSON ([`SavedBlock`]) instead of submitting it, and stop.
    pub save_block: Option<PathBuf>,
    /// Save each sealed run's pads here as `<height>.m64` (from the block's start state, not from power-on).
    pub save_dir: Option<PathBuf>,
}

impl CampaignConfig {
    pub fn new(daemon_url: &str, address: &str, rom: Vec<u8>, data_dir: PathBuf) -> CampaignConfig {
        CampaignConfig {
            daemon_url: daemon_url.into(),
            address: address.into(),
            rom,
            data_dir,
            threads: 4,
            viz: false,
            payout_view_secret: None,
            reset: "when-stuck".into(),
            hub_frames: 60_000_000,
            course_frames: 150_000_000,
            anytime: true,
            submit_lead: 5,
            normalise: Normalise::Exact,
            max_blocks: 0,
            salt: 0,
            in_process: true,
            child: None,
            rom_path: None,
            scratch: None,
            signed_file: None,
            step_seconds: 0.0,
            checkpoints: 8,
            stall: 0,
            extra_env: String::new(),
            frame_cap: 0,
            reset_after: 0,
            stamp_ahead: 0,
            save_block: None,
            save_dir: None,
        }
    }
}

/// What the campaign is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    /// No template (daemon down, bad template): retrying.
    WaitingForDaemon,
    /// Building the parent's end state.
    Preparing,
    /// The tree search (castle / course steps) for the block's run.
    Searching,
    /// Searching for a shorter run while the found one waits for the time limit.
    Improving,
    /// Sealing / normalising the proof.
    Sealing,
    /// The run is over the limit: waiting until it is valid.
    WaitingValid,
    /// Nothing to play from here (no route): waiting for a new tip.
    Idle,
    Stopped,
}

#[derive(Clone, Debug, Serialize)]
pub struct State {
    pub phase: Phase,
    /// The block height being mined (0: none yet).
    pub height: u64,
    pub detail: String,
}

/// A block the campaign submitted.
#[derive(Clone, Debug, Serialize)]
pub struct BlockEvent {
    pub height: u64,
    pub frames: u32,
    /// The node took it (or holds it until it fits: `held_secs`); false: rejected, offered as a proof record instead.
    pub accepted: bool,
    pub held_secs: u64,
    pub message: String,
}

#[derive(Clone, Debug)]
pub enum Event {
    Log(String),
    State(State),
    /// A viz frame: `kind` is `viz` (the sampler's JSON line), `state` / `edges` (base64 bytes), `pert` / `hybrid_out` (JSON)
    /// or `path` (JSON); `key` names the step.
    Viz { kind: String, key: String, payload: String },
    Block(BlockEvent),
    /// The campaign ended: None after `stop()` / `max_blocks`, Some(error) if it failed.
    Stopped(Option<String>),
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub running: bool,
    /// Emulated frames the searches have simulated since the campaign started, and the rate over the last `stats()` interval.
    pub frames: u64,
    pub fps: f64,
    pub blocks_sent: u64,
    pub blocks_rejected: u64,
    pub height: u64,
    pub phase: Option<Phase>,
}

struct Inner {
    stop: AtomicBool,
    subs: Mutex<Vec<Sender<Event>>>,
    state: Mutex<State>,
    counts: Mutex<(u64, u64)>,
    /// (frames at the last `stats()`, when)
    sample: Mutex<(u64, std::time::Instant)>,
    hub: Arc<Hub>,
    running: AtomicBool,
}

impl Inner {
    fn emit(&self, e: Event) {
        self.subs.lock().unwrap().retain(|s| s.send(e.clone()).is_ok());
    }
    fn log(&self, m: impl Into<String>) {
        self.emit(Event::Log(m.into()));
    }
    fn phase(&self, phase: Phase, height: u64, detail: impl Into<String>) {
        let s = State { phase, height, detail: detail.into() };
        *self.state.lock().unwrap() = s.clone();
        self.emit(Event::State(s));
    }
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
    /// Sleeps `secs` seconds, or until the campaign is stopped.
    fn sleep(&self, secs: u64) {
        let end = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < end && !self.stopped() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// A sealed block kept for the hold tests (`save_block`, `automine --save-block / --submit-saved / --pick-difficulty`).
#[derive(Serialize, Deserialize)]
pub struct SavedBlock {
    pub template_blob: String,
    /// v0.3: the template's seed and miner key (secret, hex), to sign at submit.
    #[serde(default)]
    pub seed_hash: String,
    #[serde(default)]
    pub key_secret: String,
    /// The key's signature of this one run (hex, 64 bytes): the seed excludes the timestamp, so a restamped block keeps it
    /// (the web test tooling can't sign in node).
    #[serde(default)]
    pub sig: String,
    pub proof: String,
    pub frames: u32,
    pub parent_ts: u64,
    pub prev_hash: String,
    pub height: u64,
}

/// A running campaign. Dropping it stops the campaign and waits for it.
pub struct Campaign {
    inner: Arc<Inner>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Campaign {
    /// Starts mining on a background thread: loads the ROM, makes `data_dir`, and returns at once (errors of the loop come
    /// as [`Event::Stopped`]). Fails if the address / ROM / reset policy is unusable.
    pub fn start(cfg: CampaignConfig) -> Result<Campaign> {
        // A test network's address (--testnet / --stagenet) is not "SM6..": the daemon checks it.
        let reset_mode: ResetMode = cfg.reset.parse().map_err(|e: String| anyhow::anyhow!(e))?;
        std::fs::create_dir_all(&cfg.data_dir)?;
        sm64pow::init(cfg.rom.clone())?;
        let inner = Arc::new(Inner {
            stop: AtomicBool::new(false),
            subs: Mutex::new(Vec::new()),
            state: Mutex::new(State { phase: Phase::Starting, height: 0, detail: String::new() }),
            counts: Mutex::new((0, 0)),
            sample: Mutex::new((0, std::time::Instant::now())),
            hub: Hub::host(),
            running: AtomicBool::new(true),
        });
        if cfg.viz {
            let i = inner.clone();
            inner.hub.enable(true, Some(Arc::new(move |kind: &str, key: &str, payload: &str| i.emit(Event::Viz { kind: kind.into(), key: key.into(), payload: payload.into() }))));
        }
        let thread = {
            let inner = inner.clone();
            std::thread::Builder::new().name("sm64coin-campaign".into()).stack_size(32 << 20).spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&inner, &cfg, reset_mode)));
                let err = match r {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(format!("{e:#}")),
                    Err(_) => Some("the campaign thread panicked".into()),
                };
                inner.hub.disable();
                inner.running.store(false, Ordering::Relaxed);
                inner.phase(Phase::Stopped, 0, err.clone().unwrap_or_default());
                inner.emit(Event::Stopped(err));
            })?
        };
        Ok(Campaign { inner, thread: Mutex::new(Some(thread)) })
    }

    /// Asks the campaign to stop: the running search ends at its next round, `join` (or the `Stopped` event) says when.
    pub fn stop(&self) {
        self.inner.stop.store(true, Ordering::Relaxed);
    }

    /// Waits for the campaign thread to end.
    pub fn join(&self) {
        if let Some(t) = self.thread.lock().unwrap().take() {
            let _ = t.join();
        }
    }

    /// The events from now on (a channel per call; a dropped receiver is forgotten).
    pub fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = channel();
        self.inner.subs.lock().unwrap().push(tx);
        rx
    }

    /// The current phase.
    pub fn state(&self) -> State {
        self.inner.state.lock().unwrap().clone()
    }

    /// Counters and the search speed since the previous `stats()` call.
    pub fn stats(&self) -> Stats {
        let n = self.inner.hub.sim_frames();
        let mut s = self.inner.sample.lock().unwrap();
        let fps = (n - s.0) as f64 / s.1.elapsed().as_secs_f64().max(1e-3);
        *s = (n, std::time::Instant::now());
        let c = *self.inner.counts.lock().unwrap();
        let st = self.state();
        Stats { running: self.inner.running.load(Ordering::Relaxed), frames: n, fps, blocks_sent: c.0, blocks_rejected: c.1, height: st.height, phase: Some(st.phase) }
    }

    /// The page's mailbox, one JSON line as the desktop app sends on the miner's stdin: `{"viz_on": bool}` (the feed's
    /// switch) or `{"mail": {"goal": .., "cmds": [..], "reqs": [..]}}` (the sampling bubble, hybrid commands, node
    /// requests; other keys are ignored). Only has an effect with `viz` on.
    pub fn mailbox(&self, json: &str) {
        self.inner.hub.input(json);
    }
}

impl Drop for Campaign {
    fn drop(&mut self) {
        self.stop();
        self.join();
    }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// The parent's end state, cached per tip.
struct ParentCache {
    tip: String,
    state: SaveState,
}

/// Smallest timestamp such that a run of `frames` is within the limit of a block stamped then (never before `from`).
pub fn valid_from(retarget: u64, parent_ts: u64, frames: u32, from: u64) -> u64 {
    let r = sm64pow::rules();
    let mut ts = from.max(parent_ts);
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

/// Watches the daemon (a new block ends the search) and the campaign's stop flag, from its own thread.
struct Watch {
    /// Ends the search (a new block, the stop flag, a restart time). `pstop` ends the parent-start search only (the
    /// `reset_after` deadline) and is raised with `stop` too.
    stop: Arc<AtomicBool>,
    pstop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    new_tip: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl Watch {
    /// `deadline` (unix s, 0: none) ends the search too; `power_on_at` (0: none) restarts a "parent" template when it passes;
    /// `p_deadline` (0: none) ends the parent-start search (`pstop`) only.
    fn spawn(inner: &Arc<Inner>, url: &str, prev: &str, deadline: u64, power_on_at: u64, p_deadline: u64) -> Watch {
        let (stop, pstop, done, new_tip) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let (s, ps, d, n) = (stop.clone(), pstop.clone(), done.clone(), new_tip.clone());
        let (inner, url, prev) = (inner.clone(), url.to_string(), prev.to_string());
        let thread = std::thread::spawn(move || {
            let dm = rpc::Daemon::new(&url);
            let mut polled = std::time::Instant::now();
            while !d.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
                if inner.stopped() {
                    s.store(true, Ordering::Relaxed);
                    ps.store(true, Ordering::Relaxed);
                    return;
                }
                if polled.elapsed() < Duration::from_secs(2) {
                    continue;
                }
                polled = std::time::Instant::now();
                if (power_on_at != 0 && now() >= power_on_at) || (deadline != 0 && now() >= deadline) {
                    s.store(true, Ordering::Relaxed);
                    ps.store(true, Ordering::Relaxed);
                    return;
                }
                if p_deadline != 0 && now() >= p_deadline {
                    ps.store(true, Ordering::Relaxed);
                }
                if dm.top_hash().map(|h| h != prev).unwrap_or(false) {
                    n.store(true, Ordering::Relaxed);
                    s.store(true, Ordering::Relaxed);
                    ps.store(true, Ordering::Relaxed);
                    return;
                }
            }
        });
        Watch { stop, pstop, done, new_tip, thread }
    }
    fn finish(self) -> bool {
        self.done.store(true, Ordering::Relaxed);
        let _ = self.thread.join();
        self.new_tip.load(Ordering::Relaxed)
    }
}

struct Ctx<'a> {
    inner: &'a Arc<Inner>,
    cfg: &'a CampaignConfig,
    daemon: rpc::Daemon,
    signed: crate::signed::SignedStore,
    parent_cache: Option<ParentCache>,
    reset_mode: ResetMode,
    attempt: u64,
    accepted: u64,
}

fn run(inner: &Arc<Inner>, cfg: &CampaignConfig, reset_mode: ResetMode) -> Result<()> {
    let signed_file = cfg.signed_file.clone().unwrap_or_else(|| cfg.data_dir.join("signed.json")).to_string_lossy().into_owned();
    let mut daemon = rpc::Daemon::new(&cfg.daemon_url);
    if let Some(v) = &cfg.payout_view_secret {
        daemon = daemon.with_payouts(crate::payout::Payouts::new(&cfg.address, v, &signed_file)?);
    }
    let mut cx = Ctx { inner, cfg, daemon, signed: crate::signed::SignedStore::open(Some(&signed_file)), parent_cache: None, reset_mode, attempt: 0, accepted: 0 };
    inner.log(format!("campaign: daemon {}, {} threads, reset {:?}", cfg.daemon_url, cfg.threads, reset_mode));
    while !inner.stopped() {
        cx.attempt += 1;
        match cx.block() {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(e) if format!("{e:#}").starts_with("fatal:") => return Err(e),
            Err(e) => {
                inner.log(format!("  {e:#}; retrying in 5 s"));
                inner.phase(Phase::WaitingForDaemon, 0, format!("{e:#}"));
                inner.sleep(5);
            }
        }
    }
    Ok(())
}

impl Ctx<'_> {
    fn log(&self, m: impl Into<String>) {
        self.inner.log(m);
    }

    fn rrt_options(&self, k: u64) -> RrtOptions {
        RrtOptions {
            threads: self.cfg.threads,
            hub_frames: self.cfg.hub_frames,
            course_frames: self.cfg.course_frames,
            seed: self.attempt.wrapping_mul(7919).wrapping_add(k.wrapping_mul(104729)).wrapping_add(self.cfg.salt),
            scratch: self.cfg.scratch.clone().unwrap_or_else(|| self.cfg.data_dir.join("rrt-scratch")),
            step_seconds: self.cfg.step_seconds,
            checkpoints: self.cfg.checkpoints,
            stall: self.cfg.stall,
            extra_env: self.cfg.extra_env.clone(),
            frame_cap: self.cfg.frame_cap,
            child: self.cfg.child.clone(),
            rom: self.cfg.rom_path.clone(),
            in_process: self.cfg.in_process,
            hub: Some(self.inner.hub.clone()),
            viz: self.cfg.viz,
            // The exchange only names the steps (sync tags) for hybrid mining; the hub answers it.
            exchange: self.cfg.viz.then(|| Arc::new(|_: &str, _: &str| None) as rrt::Exchange),
            ..Default::default()
        }
    }

    /// One template to its submission. Ok(true): `max_blocks` reached.
    fn block(&mut self) -> Result<bool> {
        let (inner, cfg) = (self.inner, self.cfg);
        let t = self.daemon.template(&cfg.address)?;
        let params = sm64pow::params(t.sm64_retarget_difficulty.max(1));
        let seed = sm64pow::Seed::from_hex(&t.sm64_seed_hash).map_err(|e| anyhow::anyhow!("bad seed in the template ({e})"))?;
        let elapsed = t.sm64_timestamp.saturating_sub(t.sm64_parent_timestamp);
        let limit_now = sm64pow::rules().max_frames(t.sm64_retarget_difficulty.max(1), elapsed);
        if (!t.sm64_rules.is_empty() && t.sm64_rules != sm64pow::rules().name()) || limit_now != t.sm64_max_frames || params.rng.random_amount != t.sm64_random_amount || params.rng.window_length != t.sm64_window_length {
            bail!("fatal: this miner's rules ({}, {} frames) differ from the daemon's ({}, {} frames): update the app or the node", sm64pow::rules().name(), limit_now, t.sm64_rules, t.sm64_max_frames);
        }
        self.log(format!("block {} on {}..  retarget {}  limit {} frames  start {}  parent {} stars {} keys", t.height, t.prev_hash.get(..12).unwrap_or(&t.prev_hash), t.sm64_retarget_difficulty, t.sm64_max_frames, t.sm64_start, t.sm64_parent_stars, t.sm64_parent_keys));
        let (pick, why) = reset::initial(self.reset_mode, &t.sm64_start, t.sm64_parent_stars);
        let mut start = if pick == reset::Pick::PowerOn { Start::PowerOn } else { Start::Parent };
        self.log(format!("  reset policy {:?}: start {:?} ({why})", self.reset_mode, start));
        let mut parent_state: Option<SaveState> = None;
        if start == Start::Parent {
            inner.phase(Phase::Preparing, t.height, "the parent's end state");
            if self.parent_cache.as_ref().is_none_or(|c| c.tip != t.prev_hash) {
                let from_node = self.daemon.end_state(&t.prev_hash).and_then(|s| rpc::state_progress(&s).map(|p| (s, p)));
                let (state, p) = match from_node {
                    Ok(x) => x,
                    Err(e0) => {
                        let c = self.daemon.cycle(Some(&t.prev_hash))?;
                        let (s, p) = rpc::cycle_end_state(&c).map_err(|e| anyhow::anyhow!("cannot build the parent's end state ({e0:#}; {e:#})"))?;
                        (s, p)
                    }
                };
                self.log(format!("  parent state -> {} stars {} keys", p.stars, p.keys));
                self.parent_cache = Some(ParentCache { tip: t.prev_hash.clone(), state });
            }
            parent_state = self.parent_cache.as_ref().map(|c| c.state.clone());
        }

        inner.phase(Phase::Searching, t.height, format!("start {start:?}"));
        let power_on_at = reset::restart_at_power_on_time(self.reset_mode, &t.sm64_start, t.sm64_power_on_at).unwrap_or(0);
        let p_deadline = if self.reset_mode == ResetMode::WhenStuck && t.sm64_start == "either" && start == Start::Parent && cfg.reset_after > 0 { now() + cfg.reset_after } else { 0 };
        let w = Watch::spawn(inner, &cfg.daemon_url, &t.tip, 0, power_on_at, p_deadline);
        let ro = self.rrt_options(0);
        let mut result: Option<Mined> = None;
        let mut no_route = None;
        let log = |m: String| inner.log(format!("  {m}"));
        let first_stop = if start == Start::Parent { &w.pstop } else { &w.stop };
        let mut r = survive(rrt::mine(start, parent_state.as_ref(), &seed, &params, &ro, first_stop, log), inner);
        let fallback = match &r {
            _ if start == Start::PowerOn => None,
            Attempt::NoRoute(why) => Some(format!("no route from the parent's end state: {why}")),
            Attempt::Gave(why) if w.pstop.load(Ordering::Relaxed) && !w.stop.load(Ordering::Relaxed) => Some(format!("parent-start search timed out ({why})")),
            Attempt::Gave(why) if t.sm64_start == "either" && !w.stop.load(Ordering::Relaxed) => Some(format!("the parent's end state didn't give progress quickly ({why})")),
            _ => None,
        };
        if let Some(why) = fallback {
            self.log(format!("  {why}"));
            if reset::after_failure(self.reset_mode, &t.sm64_start) {
                self.log("  reset policy: taking the power-on start (reset cycle)");
                start = Start::PowerOn;
                parent_state = None;
                r = survive(rrt::mine(start, None, &seed, &params, &ro, &w.stop, |m| inner.log(format!("  {m}"))), inner);
            }
        }
        match r {
            Attempt::Found(m) => result = Some(m),
            Attempt::Gave(why) => self.log(format!("  no progress: {why}")),
            Attempt::NoRoute(why) => no_route = Some(why),
        }
        let new_tip = w.finish();
        let Some(m) = result else {
            if let Some(why) = no_route {
                let wake = if t.sm64_power_on_at > now() { t.sm64_power_on_at } else { now() + 5 };
                self.log(format!("  nothing to try ({why}); power-on start allowed at {} (in {} s)", t.sm64_power_on_at, wake.saturating_sub(now())));
                inner.phase(Phase::Idle, t.height, why);
                while now() < wake.min(now() + 3600) && !inner.stopped() {
                    inner.sleep(2);
                    if self.daemon.top_hash().map(|h| h != t.tip).unwrap_or(false) {
                        break;
                    }
                }
            }
            let _ = new_tip;
            return Ok(false);
        };

        inner.phase(Phase::Sealing, t.height, format!("{} frames", m.pads.len()));
        let frames = m.pads.len() as u32;
        let proof = sm64pow::seal_proof_at(start, parent_state.as_ref(), &seed, &params, &m.pads)?;
        match sm64pow::decode_proof_any(&proof, parent_state.as_ref(), &seed, &params) {
            Ok(pr) if pr.frames == frames && pr.start == start => {
                let again = sm64pow::played_from_proof_at(parent_state.as_ref(), &seed, &params, &pr)?;
                if again != m.pads {
                    self.log("  BUG: proof does not replay to the run's pads");
                    return Ok(false);
                }
                self.log(format!("  proof sealed: {frames} frames, {} bytes, {} inputs, start {:?}", proof.len(), pr.inputs.len(), start));
            }
            other => {
                self.log(format!("  BUG: own proof does not decode: {:?}", other.map(|p| p.frames)));
                return Ok(false);
            }
        }
        if let Some(dir) = &cfg.save_dir {
            std::fs::create_dir_all(dir)?;
            let mut m64 = vec![0u8; 0x400];
            m64[..4].copy_from_slice(b"M64\x1a");
            for p in &m.pads {
                m64.extend_from_slice(&p.button.to_be_bytes());
                m64.push(p.stick_x as u8);
                m64.push(p.stick_y as u8);
            }
            std::fs::write(dir.join(format!("{}.m64", t.height)), m64)?;
        }
        if let Some(f) = &cfg.save_block {
            let proof = self.normalised(start, parent_state.as_ref(), &seed, &params, &m.pads, proof);
            let key = t.key.as_ref();
            let b = SavedBlock { template_blob: t.blocktemplate_blob.clone(), seed_hash: t.sm64_seed_hash.clone(), key_secret: key.map(|k| hex::encode(k.secret)).unwrap_or_default(), sig: match key { Some(k) => hex::encode(sm64pow::mining::sign_proof(k, &seed.0, &proof)?), None => String::new() }, proof: hex::encode(&proof), frames, parent_ts: t.sm64_parent_timestamp, prev_hash: t.prev_hash.clone(), height: t.height };
            std::fs::write(f, serde_json::to_string(&b)?)?;
            self.log(format!("  saved block {} ({frames} frames) to {}, not submitted", t.height, f.display()));
            return Ok(true);
        }
        let (m, proof, frames) = if cfg.anytime && cfg.stamp_ahead == 0 {
            match self.improve(&t, start, parent_state.as_ref(), &seed, &params, m, proof)? {
                Some(x) => x,
                None => return Ok(false), // a new block landed (or stopped): the run is void
            }
        } else {
            (m, proof, frames)
        };
        if inner.stopped() {
            return Ok(false);
        }
        inner.phase(Phase::Sealing, t.height, "normalising");
        let proof = self.normalised(start, parent_state.as_ref(), &seed, &params, &m.pads, proof);

        let retarget = t.sm64_retarget_difficulty.max(1);
        let restart = start == Start::PowerOn && t.sm64_start == "either";
        let delay = if restart { t.sm64_restart_delay } else { 0 };
        let mut ts = valid_from(retarget, t.sm64_parent_timestamp + delay, frames, now());
        if ts > now() {
            let asked = self.daemon.template_valid_at(&cfg.address, frames).map(|x| x.sm64_valid_at).unwrap_or(0);
            let asked = if asked == 0 { 0 } else { asked + delay };
            ts = ts.max(asked);
            self.log(format!("  {frames} frames is over the current limit; valid at {ts} (in {} s)", ts.saturating_sub(now())));
            inner.phase(Phase::WaitingValid, t.height, format!("valid in {} s", ts.saturating_sub(now())));
            let ahead = if cfg.stamp_ahead > 0 {
                self.log(format!("  test: submitting {} s early, stamped {ts}", cfg.stamp_ahead.min(ts.saturating_sub(now()))));
                cfg.stamp_ahead
            } else {
                cfg.submit_lead.min(sm64pow::rules::RPC_SUBMIT_AHEAD_SECONDS.saturating_sub(5))
            };
            let mut abandoned = false;
            while now() + ahead < ts {
                if inner.stopped() {
                    return Ok(false);
                }
                inner.sleep(1);
                if now() % 3 == 0 && self.daemon.top_hash().map(|h| h != t.tip).unwrap_or(false) {
                    self.log("  new block on the network: abandoning the waiting run");
                    abandoned = true;
                    break;
                }
            }
            if abandoned {
                self.publish_proof(&t, &proof, frames, now());
                return Ok(false);
            }
            ts = ts.max(now());
        } else {
            ts = now();
        }
        if let Err(prev) = self.signed.may_sign(&t.sm64_seed_hash, &proof) {
            self.log(format!("  already signed a different run ({} frames) for this seed: not signing a second one; waiting for a new block", prev.frames));
            while self.daemon.top_hash().map(|h| h == t.tip).unwrap_or(true) && !inner.stopped() {
                inner.sleep(2);
            }
            return Ok(false);
        }
        self.signed.record(&t.sm64_seed_hash, &proof, frames)?;
        let sent = self.daemon.submit(&rpc::seal_signed(&t, &proof, Some(ts))?);
        match sent {
            Ok(()) => {
                self.accepted += 1;
                inner.counts.lock().unwrap().0 += 1;
                let held = ts.saturating_sub(now());
                let msg = format!("BLOCK {} {} ({:.6} SM64 reward, {} frames = {:.1} s of play, {:.0} s to find)", t.height, if held > 0 { format!("SENT, held by the node for {held} s until it fits") } else { "ACCEPTED".into() }, t.expected_reward as f64 / 1e12, frames, frames as f64 / 30.0, m.seconds);
                self.log(format!("  {msg}"));
                inner.emit(Event::Block(BlockEvent { height: t.height, frames, accepted: true, held_secs: held, message: msg }));
                if cfg.max_blocks != 0 && self.accepted >= cfg.max_blocks {
                    return Ok(true);
                }
            }
            Err(e) => {
                inner.counts.lock().unwrap().1 += 1;
                self.log(format!("  block rejected: {e}"));
                inner.emit(Event::Block(BlockEvent { height: t.height, frames, accepted: false, held_secs: 0, message: format!("{e}") }));
                match self.daemon.submit_proof(&rpc::seal_signed(&t, &proof, Some(ts))?) {
                    Ok((v, why)) => self.log(format!("  offered as a proof for height {}: verdict {v}{}", t.height, if why.is_empty() { String::new() } else { format!(" ({why})") })),
                    Err(e) => self.log(format!("  offering it as a proof failed: {e}")),
                }
            }
        }
        Ok(false)
    }

    /// Signs (once per seed) and offers a run that cannot win as a proof record for its height.
    fn publish_proof(&mut self, t: &rpc::Template, proof: &[u8], frames: u32, ts: u64) {
        if self.signed.may_sign(&t.sm64_seed_hash, proof).is_err() || self.signed.record(&t.sm64_seed_hash, proof, frames).is_err() {
            return;
        }
        match rpc::seal_signed(t, proof, Some(ts)).and_then(|b| self.daemon.submit_proof(&b)) {
            Ok((v, why)) => self.log(format!("  published as a proof for height {}: verdict {v}{}", t.height, if why.is_empty() { String::new() } else { format!(" ({why})") })),
            Err(e) => self.log(format!("  publishing the run as a proof failed: {e}")),
        }
    }

    /// Anytime mining for a run over the current limit: fresh searches from the same start until `submit_lead` s before the
    /// best run is valid; a shorter run replaces the best. None: a new block landed (or the campaign stopped).
    #[allow(clippy::too_many_arguments)]
    fn improve(&mut self, t: &rpc::Template, start: Start, parent: Option<&SaveState>, seed: &sm64pow::Seed, params: &sm64pow::Params, m: Mined, proof: Vec<u8>) -> Result<Option<(Mined, Vec<u8>, u32)>> {
        let inner = self.inner;
        let retarget = t.sm64_retarget_difficulty.max(1);
        let restart = start == Start::PowerOn && t.sm64_start == "either";
        let delay = if restart { t.sm64_restart_delay } else { 0 };
        let valid = |frames: u32| valid_from(retarget, t.sm64_parent_timestamp + delay, frames, now());
        let mut best = (m, proof);
        let mut best_frames = best.0.pads.len() as u32;
        let mut k = 1u64;
        let ahead = self.cfg.submit_lead.min(sm64pow::rules::RPC_SUBMIT_AHEAD_SECONDS.saturating_sub(5));
        while valid(best_frames) > now() + ahead {
            if inner.stopped() {
                return Ok(None);
            }
            let deadline = valid(best_frames) - ahead;
            inner.phase(Phase::Improving, t.height, format!("best {best_frames} frames, valid in {} s", valid(best_frames).saturating_sub(now())));
            self.log(format!("  anytime: best {best_frames} frames, valid in {} s; search {k} for a shorter run until then", valid(best_frames).saturating_sub(now())));
            let mut ro = self.rrt_options(k);
            let mut keys = best.0.keys.clone();
            keys.sort();
            keys.dedup();
            ro.guide_keys = keys;
            let w = Watch::spawn(inner, &self.cfg.daemon_url, &t.tip, deadline, 0, 0);
            let r = rrt::mine(start, parent, seed, params, &ro, &w.stop, |m| inner.log(format!("    {m}")));
            let stopped = w.stop.load(Ordering::Relaxed);
            if w.finish() || inner.stopped() {
                self.log("  anytime: a new block landed (or stopped): the run is void");
                return Ok(None);
            }
            match r? {
                Attempt::Found(m2) if (m2.pads.len() as u32) < best_frames => {
                    let f2 = m2.pads.len() as u32;
                    let p2 = sm64pow::seal_proof_at(start, parent, seed, params, &m2.pads)?;
                    self.log(format!("  anytime: search {k} found {f2} frames (was {best_frames}): valid {} s sooner", valid(best_frames).saturating_sub(valid(f2))));
                    best = (m2, p2);
                    best_frames = f2;
                }
                Attempt::Found(m2) => self.log(format!("  anytime: search {k} found {} frames, not shorter", m2.pads.len())),
                _ => {}
            }
            if stopped {
                break; // the deadline
            }
            k += 1;
        }
        Ok(Some((best.0, best.1, best_frames)))
    }

    /// The sealed `proof` of the run `pads`, or a smaller one of the same run (every candidate replay-checked; the original on any doubt).
    fn normalised(&self, start: Start, parent: Option<&SaveState>, seed: &sm64pow::Seed, params: &sm64pow::Params, pads: &[sm64pow::sm64_binds::GamePad], proof: Vec<u8>) -> Vec<u8> {
        let t0 = std::time::Instant::now();
        let better = match self.cfg.normalise {
            Normalise::Off => return proof,
            Normalise::Exact => {
                use sm64pow::normalise::{Method, StickPick, seal_proof_exact_report};
                seal_proof_exact_report(start, parent, seed, params, pads, Method::Exact, StickPick::Model).map(|(b, _)| b)
            }
            Normalise::Replay => sm64pow::normalise::seal_proof_normalised_report(start, parent, seed, params, pads).map(|(b, _)| b),
        };
        match better {
            Ok(b) if b.len() < proof.len() => {
                self.log(format!("  normalised: {} -> {} bytes in {:.1} s", proof.len(), b.len(), t0.elapsed().as_secs_f64()));
                b
            }
            Ok(_) => proof,
            Err(e) => {
                self.log(format!("  normaliser failed ({e:#}); sealing the run as played"));
                proof
            }
        }
    }
}

/// A failed attempt (e.g. a start that never settled) must not end the campaign: log it and give up on this template.
fn survive(r: Result<Attempt>, inner: &Arc<Inner>) -> Attempt {
    r.unwrap_or_else(|e| {
        inner.log(format!("  attempt failed: {e:#}"));
        Attempt::Gave(format!("error: {e:#}"))
    })
}
