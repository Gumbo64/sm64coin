//! The replay queue (layer 5): blocks that arrive one at a time are replayed
//! when they are due, in parallel, outside the daemon's blockchain lock
//! (docs/DOS.md "The replay queue", docs/V0.3-NEW-CHAIN.md).
//!
//! Relay and resource policy, not consensus: nothing here changes which
//! blocks are valid, only when and whether this node spends a replay on a
//! block that was not asked for, and on which thread. The flow:
//!
//! 1. **Arrival** (the daemon, under its lock, cheap): parse, signature
//!    ([`crate::sign`]), the one-run-per-key rule ([`crate::equivocation`]),
//!    the per-source budget ([`crate::admit`]). No replay. The daemon
//!    [`submit`]s a [`Job`] (everything a replay needs, plus its due time) and
//!    keeps the block.
//! 2. **Due**: a worker takes a due job and asks the daemon what is at that
//!    height ([`Chain`], through the callback set with [`set_facts`]), then
//!    [`could_matter`] decides: a block extending the tip, or a sibling of our
//!    block with **strictly fewer frames** inside the displacement window
//!    ([`crate::pof`]), is replayed; anything else is [`Verdict::Skipped`] for
//!    free. So once an honest block exists, long fakes cost nothing.
//! 3. **Replay**, on `threads` workers (default: every core), shortest claim
//!    first with a per-source fair share: a job's priority is its claimed
//!    frames plus what its source has already had replayed (decaying with a
//!    half-life of [`FAIR_HALF_LIFE`] s). One peer's burst of claims therefore
//!    queues behind another peer's single claim, and a short claim always
//!    beats a longer one from a source of equal standing. Replays use the
//!    shared end-state cache ([`crate::cache`]): candidates for one height
//!    share the parent's state and are independent. A source whose claim
//!    turns out invalid is refused for a while ([`PENALTY_SECONDS`], doubling).
//! 4. **Ready**: the verdict goes to the daemon ([`wait_ready`]); a valid
//!    one leaves its end state and summary in the cache, so the daemon's own
//!    check under the lock ([`crate::chain::verify_block`]) is the cached
//!    fast path, and only then is the block added and relayed.
//!
//! Workers never hold the queue's lock while calling the daemon or replaying,
//! and the daemon never waits for a worker while holding its lock.
//!
//! Read next: ffi.rs.

use crate::admit::{self, Bounded, Context};
use crate::chain::{self, BlockVerdict, Parent};
use crate::seed::Seed;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

/// Jobs waiting at once, and per source.
pub const QUEUE_MAX: usize = 256;
pub const PER_SOURCE_MAX: usize = 16;
/// How fast a source's "already served" frames fade, seconds.
pub const FAIR_HALF_LIFE: f64 = 60.0;
/// A source whose claim replayed invalid is refused this long, doubling per
/// repeat, up to a day.
pub const PENALTY_SECONDS: f64 = 600.0;
/// With two or more workers, one is reserved for claims of at most this many
/// frames (about 5 minutes of play; honest winning runs are far shorter), so
/// an honest claim never waits behind a pool full of 36,000-frame replays
/// for longer than one short replay.
pub const RESERVED_FRAMES: u32 = 9000;
const IDS: usize = 8192;
/// How much longer an unknown source's claim counts when ordering (frames).
pub const UNKNOWN_SOURCE_FRAMES: f64 = 2000.0;
/// All sources that never sent a valid claim share one replay budget (frames): a burst of
/// this many, refilled at [`UNKNOWN_FRAMES_PER_MIN`]. Claims beyond it are still queued but rank
/// last ([`OVER_BUDGET_FRAMES`]): a Sybil burst from fresh addresses can't delay a claim within
/// the budget or from a known source, and is skipped once a valid block exists at its height.
pub const UNKNOWN_FRAMES: f64 = 8.0 * 36000.0;
pub const UNKNOWN_FRAMES_PER_MIN: f64 = 2.0 * 36000.0;
/// How much longer a claim queued over that budget counts when ordering.
pub const OVER_BUDGET_FRAMES: f64 = 1_000_000.0;
const SOURCES: usize = 4096;

/// What a replay of a claim needs, as known when it arrived.
#[derive(Clone)]
pub struct Job {
    pub id: [u8; 32],
    /// The parent: `Genesis` or a block and the gap (seconds) to it.
    pub parent: Parent,
    /// The parent's id (the genesis block's for `Genesis`).
    pub parent_id: [u8; 32],
    pub seed: Seed,
    /// The retarget difficulty its limit is checked at.
    pub difficulty: u64,
    pub blob: Vec<u8>,
    /// Frames the proof claims.
    pub frames: u32,
    /// The equal-length tie-break (pof.rs: canonical proof size, then cn_fast_hash(sm64_sig)).
    pub tie: crate::pof::Tie,
    /// The block's timestamp.
    pub stamp: u64,
    /// The second it is due (unix): when the run fits the limit by this node's clock.
    pub due: u64,
    /// When it arrived (unix). It became eligible at `max(arrived, due)`, and
    /// that, not the time a worker got to it, is what the displacement window
    /// and the freshness bound read ([`Queue::eligible_at`]).
    pub arrived: u64,
    /// Where it came from (None: this node's own miner or a trusted RPC).
    pub ctx: Option<Context>,
}

/// What the daemon says is at a job's height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chain {
    /// The parent is not in our main chain.
    Unknown,
    /// The parent is our tip: nothing at the height yet.
    Tip,
    /// The parent has a child in our main chain: its id and frames.
    Child { id: [u8; 32], frames: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Replayed and valid; its end state is cached.
    Valid(u32),
    /// Replayed and invalid (the reason).
    Invalid(String),
    /// Not replayed: it cannot matter (the reason).
    Skipped(&'static str),
    /// Not decided here (the parent's state is not cached, or an error): the
    /// daemon verifies it the old way, under its lock.
    Process,
}

/// Why [`submit`] did not queue a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Already queued or decided.
    Known,
    /// The queue, or this source's share of it, is full.
    Full,
    /// This source sent an invalid claim recently.
    Penalty,
    /// Its source's replay budget (admit.rs) is used up.
    Budget(&'static str),
    /// It can't be valid by its own claim (cheap arithmetic, no replay).
    Hopeless(&'static str),
}

/// Claims shorter than this are not replayed unasked (the limit's floor, 20 s of play: no star
/// is reachable that fast; a junk claim of a few hundred frames would cost a replay's fixed
/// overhead for nothing).
pub const MIN_CLAIM_FRAMES: u32 = crate::rules::STAR_CYCLE.min_frames;

/// Whether a claim could be valid at all, from what it claims (no replay): at least
/// [`MIN_CLAIM_FRAMES`], and not longer than the limit at its own stamp (a restart's limit is
/// never larger, so this never refuses a valid block).
pub fn plausible(job: &Job) -> Result<(), &'static str> {
    if job.frames < MIN_CLAIM_FRAMES {
        return Err("claims fewer frames than any star needs");
    }
    if let Parent::Block { gap_seconds, .. } = job.parent {
        if job.frames > crate::rules::active().max_frames(job.difficulty.max(1), gap_seconds) {
            return Err("claims more frames than the limit at its own timestamp");
        }
    }
    Ok(())
}

impl Job {
    /// The second this claim became eligible here: due, but not before it arrived.
    pub fn eligible(&self) -> u64 {
        self.due.max(self.arrived)
    }
}

/// Whether a due claim is worth a replay, given what we have at its height.
pub fn could_matter(c: Chain, job: &Job, now: u64) -> Result<(), &'static str> {
    match c {
        Chain::Tip => Ok(()),
        Chain::Unknown => Err("its parent is not in our chain"),
        Chain::Child { id, frames } => {
            let at = job.eligible().min(now);
            if crate::pof::restamp_preferred(&id, &job.seed.0, job.stamp, &job.id, at) {
                Ok(()) // the same run, stamped earlier: the deterministic choice between copies (pof.rs)
            } else if crate::pof::sibling_could_matter(&id, &job.seed.0, job.frames, &job.tie, job.stamp, at) {
                Ok(())
            } else if job.frames >= frames {
                Err("not strictly shorter than our block at that height")
            } else {
                Err("outside the displacement window of our block at that height")
            }
        }
    }
}

struct Entry {
    job: Job,
    queued: f64,
    /// Other sources that sent the same claim while it waited ([`Queue::also`]): if this
    /// job's source is penalised, the claim stays, attributed to one of them.
    others: Vec<Option<Context>>,
    /// Extra rank (frames) for a claim queued over the unknown sources' budget.
    over: f64,
}

#[derive(Default)]
struct State {
    waiting: Vec<Entry>,
    ready: VecDeque<([u8; 32], Verdict)>,
    /// Ids whose valid verdict the daemon has not consumed yet.
    cleared: Option<Bounded<[u8; 32], ()>>,
    /// Ids queued or recently decided.
    known: Option<Bounded<[u8; 32], ()>>,
    /// Ids queued or replayed: when they became eligible here.
    eligible: Option<Bounded<[u8; 32], u64>>,
    /// Sources that sent a claim that verified valid.
    trusted: Option<Bounded<String, ()>>,
    /// The unknown sources' shared budget ([`UNKNOWN_FRAMES`]).
    unknown: Option<admit::Bucket>,
    /// Per source: frames replayed for it (decaying), and when that was.
    served: Option<Bounded<String, (f64, f64)>>,
    /// Per source: refused until, and how often it offended.
    bad: Option<Bounded<String, (f64, u32)>>,
    per_source: HashMap<String, usize>,
    running: usize,
    stop: bool,
}

type Facts = dyn Fn(&[u8; 32]) -> Chain + Send + Sync;
type Replay = dyn Fn(&Job) -> Verdict + Send + Sync;

pub struct Queue {
    st: Mutex<State>,
    work: Condvar,
    done: Condvar,
    facts: Mutex<Option<Arc<Facts>>>,
    replay: Box<Replay>,
    threads: AtomicUsize,
    started: OnceLock<()>,
    /// Refuse implausible claims ([`plausible`]) on submit (the process's queue does; unit tests' don't).
    pub check_plausible: std::sync::atomic::AtomicBool,
    /// Counters for logs and tests.
    pub replayed: AtomicUsize,
    pub skipped: AtomicUsize,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn source_of(job: &Job) -> String {
    job.ctx.as_ref().map(|c| c.host.clone()).unwrap_or_default()
}

/// Whether the job's source gets a per-source share and penalty: not this
/// node's own (no context), and not a shared restricted-RPC address
/// ([`admit::shared_host`]: a gateway, where one bad client must not lock
/// everyone out).
fn accountable(job: &Job) -> bool {
    match &job.ctx {
        None => false,
        Some(c) => !(c.source == admit::Source::RpcSubmit && admit::shared_host(&c.host)),
    }
}

impl State {
    fn maps(&mut self) -> (&mut Bounded<[u8; 32], ()>, &mut Bounded<[u8; 32], ()>, &mut Bounded<String, (f64, f64)>, &mut Bounded<String, (f64, u32)>) {
        (
            self.cleared.get_or_insert_with(|| Bounded::new(IDS)),
            self.known.get_or_insert_with(|| Bounded::new(IDS)),
            self.served.get_or_insert_with(|| Bounded::new(SOURCES)),
            self.bad.get_or_insert_with(|| Bounded::new(SOURCES)),
        )
    }

    fn served_now(&mut self, source: &str, t: f64) -> f64 {
        match self.maps().2.get(&source.to_string()) {
            Some(&(v, at)) => v * f64::powf(0.5, (t - at).max(0.0) / FAIR_HALF_LIFE),
            None => 0.0,
        }
    }

    /// The index of the due job to replay next: least claimed frames plus
    /// what its source has had, the oldest first among equals.
    fn pick(&mut self, unix: u64, t: f64, long_ok: bool) -> Option<usize> {
        let mut best: Option<(f64, f64, usize)> = None;
        for i in 0..self.waiting.len() {
            if self.waiting[i].job.due > unix || (!long_ok && self.waiting[i].job.frames > RESERVED_FRAMES) {
                continue;
            }
            let (frames, queued, src) = (self.waiting[i].job.frames as f64, self.waiting[i].queued, source_of(&self.waiting[i].job));
            // Sources whose claims verified before rank ahead of unknown ones (an unknown source's
            // claim counts as UNKNOWN_SOURCE_FRAMES longer): a burst from fresh addresses queues
            // behind a known miner's or relay's claim of the same length.
            let unknown = if !src.is_empty() && self.trusted.as_ref().is_none_or(|m| m.get(&src).is_none()) { UNKNOWN_SOURCE_FRAMES } else { 0.0 };
            let key = (frames + self.served_now(&src, t) + unknown + self.waiting[i].over, queued, i);
            if best.is_none_or(|b| (key.0, key.1) < (b.0, b.1)) {
                best = Some(key);
            }
        }
        best.map(|b| b.2)
    }

    fn next_due(&self, long_ok: bool) -> Option<u64> {
        self.waiting.iter().filter(|e| long_ok || e.job.frames <= RESERVED_FRAMES).map(|e| e.job.due).min()
    }

    /// Takes waiting job `i` out without a replay, with a Skipped verdict for the daemon (which drops the block).
    fn drop_waiting(&mut self, i: usize, why: &'static str) {
        let e = self.waiting.swap_remove(i);
        admit::refund_replay(e.job.frames, e.job.ctx.as_ref()); // reserved on arrival, never replayed
        let src = source_of(&e.job);
        if let Some(n) = self.per_source.get_mut(&src) {
            *n = n.saturating_sub(1);
        }
        if self.per_source.get(&src) == Some(&0) {
            self.per_source.remove(&src);
        }
        self.ready.push_back((e.job.id, Verdict::Skipped(why)));
    }
}

impl Queue {
    pub fn new(threads: usize, replay: Box<Replay>) -> Arc<Queue> {
        Arc::new(Queue {
            st: Mutex::new(State::default()),
            work: Condvar::new(),
            done: Condvar::new(),
            facts: Mutex::new(None),
            replay,
            threads: AtomicUsize::new(threads.max(1)),
            started: OnceLock::new(),
            check_plausible: std::sync::atomic::AtomicBool::new(false),
            replayed: AtomicUsize::new(0),
            skipped: AtomicUsize::new(0),
        })
    }

    pub fn set_facts(&self, f: Arc<Facts>) {
        *self.facts.lock().unwrap() = Some(f);
    }

    pub fn set_threads(&self, n: usize) {
        self.threads.store(n.max(1), Ordering::Relaxed);
    }

    /// Queues `job` (until it is due, then it competes for a worker).
    ///
    /// When the queue (or the source's share of it) is full, the waiting job
    /// due last (of that source) makes room if the new one is due sooner: a
    /// claim stamped far ahead can't keep a claim that is due now out. The
    /// evicted job gets a Skipped verdict. The source's replay budget is
    /// checked here and charged only when a replay runs.
    pub fn submit(self: &Arc<Self>, job: Job) -> Result<(), Refused> {
        let t = admit::now();
        let src = source_of(&job);
        let accountable = accountable(&job);
        if job.ctx.is_some() && self.check_plausible.load(Ordering::Relaxed) {
            plausible(&job).map_err(Refused::Hopeless)?;
        }
        if let admit::Decision::Skip(why) = admit::gate_relay(job.ctx.as_ref()) {
            return Err(Refused::Budget(why));
        }
        // The claimed frames are reserved now (refunded if the claim is refused, evicted, purged
        // or skipped), so one burst can't queue more than the budget allows.
        admit::charge_replay(job.frames, job.ctx.as_ref());
        let refund = |r: Refused| {
            admit::refund_replay(job.frames, job.ctx.as_ref());
            Err(r)
        };
        let mut evicted = false;
        {
            let mut st = self.st.lock().unwrap();
            let (_, known, _, bad) = st.maps();
            if known.get(&job.id).is_some() {
                return refund(Refused::Known);
            }
            if accountable && matches!(bad.get(&src), Some(&(until, _)) if t < until) {
                return refund(Refused::Penalty);
            }
            let n = st.per_source.get(&src).copied().unwrap_or(0);
            // A shared gateway address (all web miners) may hold at most half the queue.
            let share = if accountable { PER_SOURCE_MAX } else if src.is_empty() { QUEUE_MAX } else { QUEUE_MAX / 2 };
            let source_full = n >= share;
            if source_full || st.waiting.len() >= QUEUE_MAX {
                // The victim: a claim not due within 2 minutes first, then the worst priority
                // (claimed frames plus what its source was served), never one that several sources
                // sent; and only if it ranks below the new claim.
                let unix = unix_now();
                let rank = |st: &mut State, j: &Job| ((j.due > unix + 120) as u8, (j.frames as f64 + st.served_now(&source_of(j), t)) as u64);
                let new_rank = rank(&mut st, &job);
                let mut worst: Option<(usize, (u8, u64), [u8; 32])> = None;
                for i in 0..st.waiting.len() {
                    if !st.waiting[i].others.is_empty() || (source_full && source_of(&st.waiting[i].job) != src) {
                        continue;
                    }
                    let j = st.waiting[i].job.clone();
                    let r = rank(&mut st, &j);
                    if worst.is_none_or(|w| r > w.1) {
                        worst = Some((i, r, j.id));
                    }
                }
                match worst.map(|(i, r, id)| (i, r > new_rank, id)) {
                    Some((i, true, id)) => {
                        st.drop_waiting(i, "evicted: the replay queue was full and a better claim arrived");
                        st.maps().1.remove(&id);
                        evicted = true;
                    }
                    _ => return refund(Refused::Full),
                }
            }
            // Over the unknown sources' shared budget: still queued (an honest block relayed by a
            // fresh peer must not be lost), but ranked behind everything else, so it is replayed only
            // if nothing better is waiting, and skipped for free once a valid block exists at its height.
            let mut over = 0.0;
            if accountable && st.trusted.as_ref().is_none_or(|m| m.get(&src).is_none()) {
                let b = st.unknown.get_or_insert_with(|| admit::Bucket::new(UNKNOWN_FRAMES, UNKNOWN_FRAMES_PER_MIN, t));
                if !b.positive(t) {
                    over = OVER_BUDGET_FRAMES;
                }
                b.charge(job.frames as f64, t);
            }
            st.maps().1.insert(job.id, ());
            let el = job.eligible();
            st.eligible.get_or_insert_with(|| Bounded::new(IDS)).insert(job.id, el);
            *st.per_source.entry(src).or_insert(0) += 1;
            st.waiting.push(Entry { job, queued: t, others: Vec::new(), over });
        }
        self.start();
        self.work.notify_all();
        if evicted {
            self.done.notify_all();
        }
        Ok(())
    }

    /// Another source (this thread's context) sent claim `id`, which is already waiting:
    /// remembered as a fallback source for it (at most 4).
    pub fn also(&self, id: &[u8; 32], ctx: Option<Context>) {
        let mut st = self.st.lock().unwrap();
        if let Some(e) = st.waiting.iter_mut().find(|e| e.job.id == *id) {
            let same = |c: &Option<Context>| c.as_ref().map(|c| (&c.host, &c.source)) == ctx.as_ref().map(|c| (&c.host, &c.source));
            if e.others.len() < 4 && !same(&e.job.ctx) && !e.others.iter().any(same) {
                e.others.push(ctx);
            }
        }
    }

    /// When claim `id` became eligible here (queued or replayed by this queue), if known.
    pub fn eligible_at(&self, id: &[u8; 32]) -> Option<u64> {
        self.st.lock().unwrap().eligible.as_ref().and_then(|m| m.get(id).copied())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn start(self: &Arc<Self>) {
        self.started.get_or_init(|| {
            let n = self.threads.load(Ordering::Relaxed);
            for k in 0..n {
                let q = Arc::clone(self);
                // Worker 0 is reserved for short claims when there are two or more.
                let long_ok = !(k == 0 && n >= 2);
                std::thread::Builder::new().name(format!("sm64-replay-{k}")).spawn(move || q.worker(long_ok)).expect("spawn a replay worker");
            }
        });
    }
    #[cfg(target_arch = "wasm32")]
    fn start(self: &Arc<Self>) {}

    fn worker(self: Arc<Self>, long_ok: bool) {
        loop {
            let entry = {
                let mut st = self.st.lock().unwrap();
                loop {
                    if st.stop {
                        return;
                    }
                    let (unix, t) = (unix_now(), admit::now());
                    if let Some(i) = st.pick(unix, t, long_ok) {
                        st.running += 1;
                        break st.waiting.swap_remove(i);
                    }
                    let wait = match st.next_due(long_ok) {
                        Some(d) if d > unix => Duration::from_millis(((d - unix) * 1000).clamp(20, 1000)),
                        Some(_) => Duration::from_millis(50), // due, but `pick` passed it over: look again shortly (a submit also wakes us)
                        None => Duration::from_secs(1),
                    };
                    st = self.work.wait_timeout(st, wait).unwrap().0;
                }
            };
            let verdict = self.decide(&entry.job);
            self.finish(&entry.job, verdict, true);
        }
    }

    /// The decision and the replay for one job, on the calling thread (no lock held).
    pub fn decide(&self, job: &Job) -> Verdict {
        let facts = self.facts.lock().unwrap().clone();
        if let Some(f) = facts {
            if let Err(why) = could_matter(f(&job.parent_id), job, unix_now()) {
                self.skipped.fetch_add(1, Ordering::Relaxed);
                admit::refund_replay(job.frames, job.ctx.as_ref());
                return Verdict::Skipped(why);
            }
        }
        self.replayed.fetch_add(1, Ordering::Relaxed);
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.replay)(job))).unwrap_or(Verdict::Process)
    }

    fn finish(&self, job: &Job, verdict: Verdict, queued: bool) {
        let (t, src) = (admit::now(), source_of(job));
        let mut st = self.st.lock().unwrap();
        if queued {
            st.running -= 1;
            if let Some(n) = st.per_source.get_mut(&src) {
                *n = n.saturating_sub(1);
            }
            if st.per_source.get(&src) == Some(&0) {
                st.per_source.remove(&src);
            }
        }
        if !matches!(verdict, Verdict::Skipped(_)) {
            let v = st.served_now(&src, t) + job.frames as f64;
            st.maps().2.insert(src.clone(), (v, t));
        }
        let mut purged = false;
        match &verdict {
            Verdict::Invalid(_) if accountable(job) => {
                let times = st.maps().3.get(&src).map_or(0, |b| b.1);
                let secs = (PENALTY_SECONDS * f64::powi(2.0, times.min(8) as i32)).min(86400.0);
                st.maps().3.insert(src.clone(), (t + secs, times + 1));
                // Its other waiting claims go too, without a replay, unless another source sent the
                // same claim (an attacker relaying an honest block first must not get it dropped).
                let mut i = 0;
                while i < st.waiting.len() {
                    if accountable(&st.waiting[i].job) && source_of(&st.waiting[i].job) == src {
                        let others = std::mem::take(&mut st.waiting[i].others);
                        let base = st.waiting[i].job.clone();
                        let pick = others.into_iter().find(|c| {
                            let probe = Job { ctx: c.clone(), ..base.clone() };
                            let h = source_of(&probe);
                            !accountable(&probe) || !matches!(st.bad.as_ref().and_then(|b| b.get(&h)), Some(&(until, _)) if t < until)
                        });
                        if let Some(c) = pick {
                            let old = source_of(&st.waiting[i].job);
                            st.waiting[i].job.ctx = c;
                            let new = source_of(&st.waiting[i].job);
                            if let Some(n) = st.per_source.get_mut(&old) {
                                *n = n.saturating_sub(1);
                            }
                            *st.per_source.entry(new).or_insert(0) += 1;
                            i += 1;
                            continue;
                        }
                        let id = st.waiting[i].job.id;
                        st.drop_waiting(i, "its source sent an invalid claim");
                        st.maps().1.remove(&id); // another peer may still send it
                        purged = true;
                    } else {
                        i += 1;
                    }
                }
            }
            Verdict::Valid(_) => {
                st.maps().0.insert(job.id, ());
                if !src.is_empty() {
                    st.trusted.get_or_insert_with(|| Bounded::new(SOURCES)).insert(src.clone(), ());
                }
            }
            _ => {}
        }
        if queued {
            st.ready.push_back((job.id, verdict));
        }
        drop(st);
        if queued || purged {
            self.done.notify_all();
        }
    }

    /// Replays `job` now, on the calling thread (a trusted caller outside any
    /// lock: the local miner's `submit_block`), and records the verdict like a
    /// queued job's (a valid one is [`Queue::take_cleared`]).
    pub fn replay_now(&self, job: Job) -> Verdict {
        {
            let mut st = self.st.lock().unwrap();
            if st.maps().1.get(&job.id).is_some() {
                return Verdict::Process; // already queued or decided: the daemon's normal path sorts it out
            }
            st.maps().1.insert(job.id, ());
            let el = job.eligible();
            st.eligible.get_or_insert_with(|| Bounded::new(IDS)).insert(job.id, el);
        }
        let v = self.decide(&job);
        self.finish(&job, v.clone(), false);
        v
    }

    /// The next verdict for the daemon, waiting up to `timeout`.
    pub fn wait_ready(&self, timeout: Duration) -> Option<([u8; 32], Verdict)> {
        let mut st = self.st.lock().unwrap();
        if st.ready.is_empty() {
            st = self.done.wait_timeout(st, timeout).unwrap().0;
        }
        st.ready.pop_front()
    }

    /// Whether `id`'s valid verdict is waiting to be consumed (and consumes it).
    pub fn take_cleared(&self, id: &[u8; 32]) -> bool {
        self.st.lock().unwrap().maps().0.remove(id).is_some()
    }

    pub fn waiting(&self) -> usize {
        self.st.lock().unwrap().waiting.len()
    }

    /// Stops the workers (tests).
    pub fn stop(&self) {
        self.st.lock().unwrap().stop = true;
        self.work.notify_all();
    }
}

/// The process's queue: replays with [`chain::verify_block`].
pub fn global() -> &'static Arc<Queue> {
    static Q: OnceLock<Arc<Queue>> = OnceLock::new();
    Q.get_or_init(|| {
        let n = std::thread::available_parallelism().map_or(4, |n| n.get());
        let q = Queue::new(n, Box::new(replay_job));
        q.check_plausible.store(true, Ordering::Relaxed);
        q
    })
}

fn replay_job(job: &Job) -> Verdict {
    // Replays inside the job's context: invalid verdicts are counted for its source (admit.rs).
    let _g = admit::enter(job.ctx.clone());
    match chain::verify_block(&job.id, job.parent, &job.seed, job.difficulty, &job.blob) {
        Ok(BlockVerdict::Valid { frames, .. }) => Verdict::Valid(frames),
        Ok(BlockVerdict::Invalid(why)) => Verdict::Invalid(why),
        Ok(BlockVerdict::NeedParent) | Err(_) => Verdict::Process,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn ctx(host: &str) -> Option<Context> {
        Some(Context { host: host.into(), source: admit::Source::Relay })
    }

    fn job(n: u8, frames: u32, due: u64, host: &str) -> Job {
        Job { id: [n; 32], parent: Parent::Genesis, parent_id: [0; 32], seed: Seed([n; 32]), difficulty: 1, blob: vec![], frames, tie: crate::pof::NO_TIE, stamp: due, due, arrived: due, ctx: ctx(host) }
    }

    fn valid_replay() -> Box<Replay> {
        Box::new(|j: &Job| Verdict::Valid(j.frames))
    }

    fn drain(q: &Queue, n: usize) -> Vec<([u8; 32], Verdict)> {
        let mut out = Vec::new();
        for _ in 0..n * 50 {
            if let Some(r) = q.wait_ready(Duration::from_millis(100)) {
                out.push(r);
                if out.len() == n {
                    break;
                }
            }
        }
        out
    }

    #[test]
    fn due_jobs_run_shortest_first_with_a_fair_share_per_source() {
        let mut st = State::default();
        let t = 100.0;
        // One peer sends three 3000-frame claims, another one 3000: the others queue behind it once the first has been served.
        for (i, (f, h)) in [(3000, "a"), (3000, "a"), (3000, "a"), (3000, "b"), (9000, "c"), (500, "d")].into_iter().enumerate() {
            st.waiting.push(Entry { job: job(i as u8, f, 10, h), queued: t + i as f64, others: vec![], over: 0.0 });
        }
        st.waiting.push(Entry { job: job(99, 100, 1_000_000, "z"), queued: t, others: vec![], over: 0.0 }); // not due
        let first = st.pick(10, t, true).unwrap();
        assert_eq!(st.waiting[first].job.frames, 500, "the shortest claim first");
        st.waiting.swap_remove(first);
        let first = st.pick(10, t, true).unwrap();
        assert_eq!(st.waiting[first].job.frames, 3000);
        let src = source_of(&st.waiting[first].job);
        st.waiting.swap_remove(first);
        // That source had 3000 frames replayed: its other claims now rank behind every other 3000-frame claim.
        st.maps().2.insert(src.clone(), (3000.0, t));
        let next = st.pick(10, t, true).unwrap();
        assert_ne!(source_of(&st.waiting[next].job), src, "the fair share: another source's claim goes first");
        // The share fades with time.
        assert!(st.served_now(&src, t + 3.0 * FAIR_HALF_LIFE) < 400.0);
        // Nothing is due yet for the last one.
        let mut only = State::default();
        only.waiting.push(Entry { job: job(1, 10, 1_000_000, "x"), queued: 0.0, others: vec![], over: 0.0 });
        assert!(only.pick(10, t, true).is_none());
        assert_eq!(only.next_due(true), Some(1_000_000));
    }

    #[test]
    fn what_could_matter() {
        let j = job(1, 2000, unix_now(), "a");
        let now = unix_now();
        assert_eq!(could_matter(Chain::Tip, &j, now), Ok(()), "extends the tip");
        assert!(could_matter(Chain::Unknown, &j, now).is_err());
        let ours = [7u8; 32];
        // Not shorter than our block at that height: skipped for free, whatever else.
        assert!(could_matter(Chain::Child { id: ours, frames: 2000 }, &j, now).is_err());
        assert!(could_matter(Chain::Child { id: ours, frames: 1500 }, &j, now).is_err());
        // Strictly shorter, but our block was never seen eligible (no window): skipped.
        assert!(could_matter(Chain::Child { id: ours, frames: 2500 }, &j, now).is_err());
        // Strictly shorter inside the window of our block: replayed.
        crate::pof::policy().lock().unwrap().note_eligible(ours, [3; 32], [4; 32], 2500, now, now);
        assert_eq!(could_matter(Chain::Child { id: ours, frames: 2500 }, &j, now), Ok(()));
        // The same run arriving 10 minutes later: outside the window.
        let late = job(1, 2000, now + 600, "a");
        assert!(could_matter(Chain::Child { id: ours, frames: 2500 }, &late, now + 600).is_err());
        // But a claim that arrived (and was due) inside the window still counts when a slow worker gets to it later.
        assert_eq!(could_matter(Chain::Child { id: ours, frames: 2500 }, &j, now + 600), Ok(()));
    }

    #[test]
    fn jobs_wait_for_their_due_time_replay_in_parallel_and_never_block_submit() {
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (r, p) = (running.clone(), peak.clone());
        let q = Queue::new(
            4,
            Box::new(move |j: &Job| {
                let n = r.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(n, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(300));
                r.fetch_sub(1, Ordering::SeqCst);
                Verdict::Valid(j.frames)
            }),
        );
        let now = unix_now();
        let t0 = std::time::Instant::now();
        for i in 0..4 {
            q.submit(job(i, 1000 + i as u32, now, &format!("h{i}"))).unwrap();
        }
        assert!(t0.elapsed() < Duration::from_millis(100), "submit doesn't wait for replays");
        q.submit(job(50, 10, now + 2, "late")).unwrap();
        let got = drain(&q, 4);
        assert_eq!(got.len(), 4);
        assert!(peak.load(Ordering::SeqCst) >= 3, "replays ran in parallel (peak {})", peak.load(Ordering::SeqCst));
        assert!(got.iter().all(|(_, v)| matches!(v, Verdict::Valid(_))));
        // The late one only after its due second.
        let (id, _) = drain(&q, 1).pop().expect("the late job");
        assert_eq!(id, [50; 32]);
        assert!(unix_now() >= now + 2);
        assert!(q.take_cleared(&[50; 32]) && !q.take_cleared(&[50; 32]), "a valid verdict is consumed once");
        q.stop();
    }

    #[test]
    fn duplicates_caps_and_penalty() {
        let q = Queue::new(1, Box::new(|_: &Job| Verdict::Invalid("bad".into())));
        let far = unix_now() + 100_000;
        q.submit(job(1, 100, far, "a")).unwrap();
        assert_eq!(q.submit(job(1, 100, far, "a")), Err(Refused::Known));
        for i in 0..PER_SOURCE_MAX - 1 {
            q.submit(job(10 + i as u8, 100, far, "a")).unwrap();
        }
        assert_eq!(q.submit(job(200, 100, far, "a")), Err(Refused::Full), "a source's share is bounded");
        q.submit(job(201, 100, far, "b")).unwrap();
        // An invalid claim penalises its source; the local source (no context) is never penalised.
        let now = unix_now();
        q.submit(job(60, 100, now, "evil")).unwrap();
        let (id, v) = drain(&q, 1).pop().unwrap();
        assert_eq!((id, v), ([60; 32], Verdict::Invalid("bad".into())));
        assert_eq!(q.submit(job(61, 100, now, "evil")), Err(Refused::Penalty));
        let mut local = job(62, 100, now, "");
        local.ctx = None;
        q.submit(local.clone()).unwrap();
        drain(&q, 1);
        local.id = [63; 32];
        q.submit(local).unwrap();
        q.stop();
    }

    #[test]
    fn a_full_queue_makes_room_for_a_claim_due_sooner_and_a_bad_source_loses_its_waiting_claims() {
        let q = Queue::new(1, Box::new(|_: &Job| Verdict::Invalid("bad".into())));
        let now = unix_now();
        // One source fills its share with claims due far ahead; a claim of its own due now evicts the latest.
        for i in 0..PER_SOURCE_MAX {
            q.submit(job(i as u8, 100, now + 200 + i as u64, "far")).unwrap();
        }
        assert_eq!(q.submit(job(100, 100, now + 10_000, "far")), Err(Refused::Full), "not sooner: refused");
        q.submit(job(101, 100, now + 100, "far")).unwrap();
        let (id, v) = q.wait_ready(Duration::from_secs(2)).expect("the evicted claim's verdict");
        assert!(id[0] < PER_SOURCE_MAX as u8, "one of the far-ahead claims");
        assert!(matches!(v, Verdict::Skipped(_)));
        // A claim of that source turns out invalid: its other waiting claims are dropped without a replay.
        q.submit(job(102, 100, now, "far")).unwrap();
        let got = drain(&q, PER_SOURCE_MAX + 1);
        assert!(got.iter().any(|(id, v)| *id == [102; 32] && matches!(v, Verdict::Invalid(_))));
        assert_eq!(got.iter().filter(|(_, v)| matches!(v, Verdict::Skipped(_))).count(), PER_SOURCE_MAX, "{got:?}");
        assert_eq!(q.waiting(), 0);
        q.stop();
    }

    #[test]
    fn an_attacker_relaying_an_honest_claim_first_cannot_get_it_dropped() {
        // The attacker relays the honest claim (due later), then gets itself penalised with a junk claim due now.
        let q = Queue::new(1, Box::new(|j: &Job| if j.frames == 666 { Verdict::Invalid("bad".into()) } else { Verdict::Valid(j.frames) }));
        let now = unix_now();
        q.submit(job(1, 4000, now + 2, "attacker")).unwrap();
        q.also(&[1; 32], ctx("honest"));
        q.submit(job(2, 666, now, "attacker")).unwrap();
        let got = drain(&q, 2);
        assert!(got.iter().any(|(id, v)| *id == [2; 32] && matches!(v, Verdict::Invalid(_))));
        assert!(got.iter().any(|(id, v)| *id == [1; 32] && matches!(v, Verdict::Valid(4000))), "the honest claim stays, attributed to the honest relay: {got:?}");
        // Without another source it is dropped, but may be sent again.
        q.submit(job(3, 4000, now + 2, "attacker2")).unwrap();
        q.submit(job(4, 666, now, "attacker2")).unwrap();
        let got = drain(&q, 2);
        assert!(got.iter().any(|(id, v)| *id == [3; 32] && matches!(v, Verdict::Skipped(_))));
        q.submit(job(3, 4000, now + 2, "honest2")).expect("re-sendable by another peer");
        q.stop();
    }

    #[test]
    fn hopeless_claims_cost_nothing() {
        let q = Queue::new(1, valid_replay());
        q.check_plausible.store(true, Ordering::Relaxed);
        let now = unix_now();
        assert!(matches!(q.submit(job(1, 300, now, "a")), Err(Refused::Hopeless(_))), "below any star's length");
        let mut long = job(2, 36000, now, "a");
        long.parent = Parent::Block { id: [9; 32], gap_seconds: 10 };
        assert!(matches!(q.submit(long), Err(Refused::Hopeless(_))), "over the limit at its own stamp");
        let mut ok = job(3, 3000, now, "a");
        ok.parent = Parent::Block { id: [9; 32], gap_seconds: 100_000 };
        q.submit(ok).unwrap();
        q.stop();
    }

    #[test]
    fn known_sources_rank_ahead_of_unknown_ones() {
        let mut st = State::default();
        st.waiting.push(Entry { job: job(1, 4000, 10, "fresh"), queued: 0.0, others: vec![], over: 0.0 });
        st.waiting.push(Entry { job: job(2, 4000, 10, "known"), queued: 1.0, others: vec![], over: 0.0 });
        st.trusted.get_or_insert_with(|| Bounded::new(16)).insert("known".to_string(), ());
        let i = st.pick(10, 0.0, true).unwrap();
        assert_eq!(source_of(&st.waiting[i].job), "known");
    }

    #[test]
    fn a_shared_gateway_address_is_never_locked_out() {
        // Restricted RPC behind a local tunnel: every web miner is 127.0.0.1. One bad claim must not penalise it.
        let q = Queue::new(1, Box::new(|j: &Job| if j.frames == 666 { Verdict::Invalid("bad".into()) } else { Verdict::Valid(j.frames) }));
        let now = unix_now();
        let gw = |n: u8, frames: u32| Job { ctx: Some(Context { host: "127.0.0.1".into(), source: admit::Source::RpcSubmit }), ..job(n, frames, now, "") };
        q.submit(gw(1, 666)).unwrap();
        assert!(matches!(drain(&q, 1).pop(), Some((_, Verdict::Invalid(_)))));
        q.submit(gw(2, 3000)).unwrap();
        assert!(matches!(drain(&q, 1).pop(), Some((_, Verdict::Valid(3000)))), "the next web miner's claim is replayed");
        // No per-source share either: more than PER_SOURCE_MAX claims can wait.
        for i in 0..PER_SOURCE_MAX as u8 + 4 {
            q.submit(gw(10 + i, 100)).map(|_| ()).unwrap_or_else(|e| panic!("{e:?}"));
        }
        drain(&q, PER_SOURCE_MAX + 4);
        assert!(admit::policy().lock().unwrap().rpc_submit_allowed("127.0.0.1", admit::now()));
        q.stop();
    }

    #[test]
    fn one_worker_is_kept_for_short_claims() {
        let mut st = State::default();
        st.waiting.push(Entry { job: job(1, 36000, 10, "a"), queued: 0.0, others: vec![], over: 0.0 });
        assert!(st.pick(10, 0.0, false).is_none(), "the reserved worker leaves a long claim alone");
        assert!(st.pick(10, 0.0, true).is_some());
        st.waiting.push(Entry { job: job(2, 4000, 10, "b"), queued: 1.0, others: vec![], over: 0.0 });
        let i = st.pick(10, 0.0, false).unwrap();
        assert_eq!(st.waiting[i].job.frames, 4000);
    }

    #[test]
    fn eligibility_is_arrival_or_due_not_when_a_worker_got_to_it() {
        let q = Queue::new(1, valid_replay());
        let mut j = job(7, 100, 1000, "a");
        j.arrived = 1500;
        q.submit(j.clone()).unwrap();
        assert_eq!(q.eligible_at(&[7; 32]), Some(1500));
        q.stop();
    }

    #[test]
    fn skipped_claims_cost_no_replay_and_the_facts_are_asked_outside_the_lock() {
        let replays = Arc::new(AtomicUsize::new(0));
        let r = replays.clone();
        let q = Queue::new(2, Box::new(move |j: &Job| {
            r.fetch_add(1, Ordering::SeqCst);
            Verdict::Valid(j.frames)
        }));
        let qq = Arc::downgrade(&q);
        let asked = Arc::new(AtomicBool::new(false));
        let a = asked.clone();
        // The callback takes the queue's own lock (as a daemon thread would while submitting): a deadlock if a worker held it.
        q.set_facts(Arc::new(move |_p: &[u8; 32]| {
            if let Some(q) = qq.upgrade() {
                let _ = q.waiting();
            }
            a.store(true, Ordering::SeqCst);
            Chain::Child { id: [9; 32], frames: 100 }
        }));
        let now = unix_now();
        for i in 0..5 {
            q.submit(job(i, 5000 + i as u32, now, "fake")).unwrap();
        }
        let got = drain(&q, 5);
        assert_eq!(got.len(), 5);
        assert!(got.iter().all(|(_, v)| matches!(v, Verdict::Skipped(_))), "{got:?}");
        assert_eq!(replays.load(Ordering::SeqCst), 0, "long fakes against an honest block cost no replay");
        assert!(asked.load(Ordering::SeqCst));
        q.stop();
    }

    #[test]
    fn replay_now_runs_on_the_caller_and_clears_the_block() {
        let q = Queue::new(1, valid_replay());
        let v = q.replay_now(job(5, 321, unix_now(), ""));
        assert_eq!(v, Verdict::Valid(321));
        assert!(q.take_cleared(&[5; 32]));
        assert_eq!(q.replay_now(job(5, 321, unix_now(), "")), Verdict::Process, "known: the daemon's normal path");
    }
}
