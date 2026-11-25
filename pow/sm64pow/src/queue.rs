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
const IDS: usize = 8192;
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
    /// The block's timestamp.
    pub stamp: u64,
    /// The second it is due (unix): when the run fits the limit by this node's clock.
    pub due: u64,
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
}

/// Whether a due claim is worth a replay, given what we have at its height.
pub fn could_matter(c: Chain, job: &Job, now: u64) -> Result<(), &'static str> {
    match c {
        Chain::Tip => Ok(()),
        Chain::Unknown => Err("its parent is not in our chain"),
        Chain::Child { id, frames } => {
            if job.frames >= frames {
                Err("not strictly shorter than our block at that height")
            } else if !crate::pof::policy().lock().unwrap_or_else(|e| e.into_inner()).candidate(&id, job.frames, job.stamp, now) {
                Err("outside the displacement window of our block at that height")
            } else {
                Ok(())
            }
        }
    }
}

struct Entry {
    job: Job,
    queued: f64,
}

#[derive(Default)]
struct State {
    waiting: Vec<Entry>,
    ready: VecDeque<([u8; 32], Verdict)>,
    /// Ids whose valid verdict the daemon has not consumed yet.
    cleared: Option<Bounded<[u8; 32], ()>>,
    /// Ids queued or recently decided.
    known: Option<Bounded<[u8; 32], ()>>,
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
    fn pick(&mut self, unix: u64, t: f64) -> Option<usize> {
        let mut best: Option<(f64, f64, usize)> = None;
        for i in 0..self.waiting.len() {
            if self.waiting[i].job.due > unix {
                continue;
            }
            let (frames, queued, src) = (self.waiting[i].job.frames as f64, self.waiting[i].queued, source_of(&self.waiting[i].job));
            let key = (frames + self.served_now(&src, t), queued, i);
            if best.is_none_or(|b| (key.0, key.1) < (b.0, b.1)) {
                best = Some(key);
            }
        }
        best.map(|b| b.2)
    }

    fn next_due(&self) -> Option<u64> {
        self.waiting.iter().map(|e| e.job.due).min()
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
    pub fn submit(self: &Arc<Self>, job: Job) -> Result<(), Refused> {
        let t = admit::now();
        let src = source_of(&job);
        {
            let mut st = self.st.lock().unwrap();
            let (_, known, _, bad) = st.maps();
            if known.get(&job.id).is_some() {
                return Err(Refused::Known);
            }
            if !src.is_empty() && matches!(bad.get(&src), Some(&(until, _)) if t < until) {
                return Err(Refused::Penalty);
            }
            let n = st.per_source.get(&src).copied().unwrap_or(0);
            if st.waiting.len() >= QUEUE_MAX || (!src.is_empty() && n >= PER_SOURCE_MAX) {
                return Err(Refused::Full);
            }
            st.maps().1.insert(job.id, ());
            *st.per_source.entry(src).or_insert(0) += 1;
            st.waiting.push(Entry { job, queued: t });
        }
        self.start();
        self.work.notify_all();
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn start(self: &Arc<Self>) {
        self.started.get_or_init(|| {
            for k in 0..self.threads.load(Ordering::Relaxed) {
                let q = Arc::clone(self);
                std::thread::Builder::new().name(format!("sm64-replay-{k}")).spawn(move || q.worker()).expect("spawn a replay worker");
            }
        });
    }
    #[cfg(target_arch = "wasm32")]
    fn start(self: &Arc<Self>) {}

    fn worker(self: Arc<Self>) {
        loop {
            let entry = {
                let mut st = self.st.lock().unwrap();
                loop {
                    if st.stop {
                        return;
                    }
                    let (unix, t) = (unix_now(), admit::now());
                    if let Some(i) = st.pick(unix, t) {
                        st.running += 1;
                        break st.waiting.swap_remove(i);
                    }
                    let wait = match st.next_due() {
                        Some(d) => Duration::from_millis(((d.saturating_sub(unix)) * 1000).clamp(20, 1000)),
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
        match &verdict {
            Verdict::Invalid(_) if !src.is_empty() => {
                let times = st.maps().3.get(&src).map_or(0, |b| b.1);
                let secs = (PENALTY_SECONDS * f64::powi(2.0, times.min(8) as i32)).min(86400.0);
                st.maps().3.insert(src, (t + secs, times + 1));
            }
            Verdict::Valid(_) => {
                st.maps().0.insert(job.id, ());
            }
            _ => {}
        }
        if queued {
            st.ready.push_back((job.id, verdict));
            drop(st);
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
        Queue::new(n, Box::new(replay_job))
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
        Job { id: [n; 32], parent: Parent::Genesis, parent_id: [0; 32], seed: Seed([n; 32]), difficulty: 1, blob: vec![], frames, stamp: due, due, ctx: ctx(host) }
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
            st.waiting.push(Entry { job: job(i as u8, f, 10, h), queued: t + i as f64 });
        }
        st.waiting.push(Entry { job: job(99, 100, 1_000_000, "z"), queued: t }); // not due
        let first = st.pick(10, t).unwrap();
        assert_eq!(st.waiting[first].job.frames, 500, "the shortest claim first");
        st.waiting.swap_remove(first);
        let first = st.pick(10, t).unwrap();
        assert_eq!(st.waiting[first].job.frames, 3000);
        let src = source_of(&st.waiting[first].job);
        st.waiting.swap_remove(first);
        // That source had 3000 frames replayed: its other claims now rank behind every other 3000-frame claim.
        st.maps().2.insert(src.clone(), (3000.0, t));
        let next = st.pick(10, t).unwrap();
        assert_ne!(source_of(&st.waiting[next].job), src, "the fair share: another source's claim goes first");
        // The share fades with time.
        assert!(st.served_now(&src, t + 3.0 * FAIR_HALF_LIFE) < 400.0);
        // Nothing is due yet for the last one.
        let mut only = State::default();
        only.waiting.push(Entry { job: job(1, 10, 1_000_000, "x"), queued: 0.0 });
        assert!(only.pick(10, t).is_none());
        assert_eq!(only.next_due(), Some(1_000_000));
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
        // The same run 10 minutes later: outside the window.
        assert!(could_matter(Chain::Child { id: ours, frames: 2500 }, &j, now + 600).is_err());
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
