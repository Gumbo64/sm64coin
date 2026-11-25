//! Admission policy (layer 5): which blocks earn a replay (docs/DOS.md).
//!
//! Relay and resource policy, not consensus: nothing here changes which
//! blocks are valid, only which ones this node spends a replay on, and when.
//! A replay costs 0.05-1 s of CPU here (more on a Raspberry Pi), against a
//! hash check for a proof-of-work coin, and anyone can mint *valid* blocks
//! for free by changing the last inputs of a published run (the variants
//! keep the seed and the length, but have a new block id). So the daemon asks
//! this module before it replays a block it was not asked for:
//!
//! - [`admit_alt`]: a block that does not extend the tip (an alternative
//!   block). One that cannot reorganise the chain by itself (not above the
//!   tip) is replayed only if it carries a template we have not seen at that
//!   height, at most [`ALT_PER_PARENT`] such per parent, or when the sync
//!   batch it came in reaches above our tip. A variant or re-stamped copy of
//!   a block we know is never replayed for nothing; it is replayed when a
//!   longer chain is built on it (it then arrives in a sync batch with its
//!   descendants).
//! - [`admit_relay`]: every block that arrives unasked (a relayed block, a
//!   block submitted over restricted RPC) is charged its claimed frames to a
//!   per-source budget ([`SOURCE_FRAMES`]); restricted RPC also has a global
//!   budget, and a source that submitted an invalid proof over RPC is refused
//!   for a while (P2P peers are banned by the daemon, as for a bad PoW).
//! - [`RpcScope`]: replays that RPC calls cause (played pads, cycles,
//!   templates on old blocks) draw on a replay-seconds budget; with none left
//!   an RPC replay fails (`busy`) and only cached answers are served.
//!
//! The daemon tells us where a block came from with [`Context`] (set per
//! thread around the handling of a P2P message or an RPC call). Blocks
//! processed without a context (held blocks becoming due, rebuilds,
//! internal) are never refused.
//!
//! Read next: chain.rs (where verdicts feed [`note_valid`] and [`note_invalid`]).

use crate::seed::Seed;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Mutex, OnceLock};

/// Alternative blocks with a new template that one parent may get replayed
/// (unasked, not reaching above the tip). Two covers an honest race of three
/// miners; more siblings are useless to fork choice until something is built
/// on them, and then they come in a sync batch.
pub const ALT_PER_PARENT: u32 = 2;
/// Per source of unasked blocks: a burst of this many frames (four blocks at
/// the longest limit), refilled at [`SOURCE_FRAMES_PER_MIN`].
pub const SOURCE_FRAMES: f64 = 4.0 * 36000.0;
pub const SOURCE_FRAMES_PER_MIN: f64 = 36000.0;
/// Restricted RPC, all sources together: 20 longest blocks, one per 10 s.
pub const RPC_GLOBAL_FRAMES: f64 = 20.0 * 36000.0;
pub const RPC_GLOBAL_FRAMES_PER_MIN: f64 = 6.0 * 36000.0;
/// A restricted-RPC source that submitted an invalid proof is refused for
/// this long, doubling with every repeat, up to a day.
pub const RPC_PENALTY_SECONDS: f64 = 600.0;
/// Replays caused by RPC calls: a burst of this many seconds of CPU, refilled
/// at [`RPC_REPLAY_SECONDS_PER_MIN`] (a fifth of one core).
pub const RPC_REPLAY_SECONDS: f64 = 20.0;
pub const RPC_REPLAY_SECONDS_PER_MIN: f64 = 12.0;
/// Bounds on the bookkeeping.
const SEEDS: usize = 65536;
const PARENTS: usize = 16384;
const SOURCES: usize = 4096;

/// Seconds on a monotonic clock (tests drive their own clock: [`Policy`]
/// methods take `now`).
pub fn now() -> f64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static T0: OnceLock<std::time::Instant> = OnceLock::new();
        T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
    }
    #[cfg(target_arch = "wasm32")]
    {
        0.0
    }
}

/// A map that forgets its oldest entries beyond `cap`.
pub struct Bounded<K, V> {
    map: HashMap<K, V>,
    order: VecDeque<K>,
    cap: usize,
}

impl<K: Hash + Eq + Clone, V> Bounded<K, V> {
    pub fn new(cap: usize) -> Self {
        Bounded { map: HashMap::new(), order: VecDeque::new(), cap }
    }
    pub fn get(&self, k: &K) -> Option<&V> {
        self.map.get(k)
    }
    pub fn get_mut(&mut self, k: &K) -> Option<&mut V> {
        self.map.get_mut(k)
    }
    pub fn insert(&mut self, k: K, v: V) {
        if self.map.insert(k.clone(), v).is_none() {
            self.order.push_back(k);
        }
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
    pub fn entry_or(&mut self, k: K, f: impl FnOnce() -> V) -> &mut V {
        if !self.map.contains_key(&k) {
            self.insert(k.clone(), f());
        }
        self.map.get_mut(&k).unwrap()
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// A token bucket: `cap` tokens, refilled at `rate` per second.
#[derive(Clone, Copy, Debug)]
pub struct Bucket {
    pub cap: f64,
    pub rate: f64,
    pub tokens: f64,
    pub last: f64,
}

impl Bucket {
    pub fn new(cap: f64, per_minute: f64, now: f64) -> Self {
        Bucket { cap, rate: per_minute / 60.0, tokens: cap, last: now }
    }
    fn refill(&mut self, now: f64) {
        if now > self.last {
            self.tokens = (self.tokens + (now - self.last) * self.rate).min(self.cap);
            self.last = now;
        }
    }
    /// Takes `n` tokens if there are that many.
    pub fn take(&mut self, n: f64, now: f64) -> bool {
        self.refill(now);
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }
    /// Whether any token is left (then [`Bucket::charge`] may go negative).
    pub fn positive(&mut self, now: f64) -> bool {
        self.refill(now);
        self.tokens > 0.0
    }
    pub fn charge(&mut self, n: f64, now: f64) {
        self.refill(now);
        self.tokens -= n;
    }
}

/// Where the block (or RPC call) being handled on this thread came from.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A block a peer relayed to us, unasked (NOTIFY_NEW_FLUFFY_BLOCK).
    Relay,
    /// Blocks we asked a peer for (a sync span); `top` is the height of the
    /// span's last block.
    Sync { top: u64 },
    /// A block submitted over restricted (public) RPC.
    RpcSubmit,
}

#[derive(Clone, Debug)]
pub struct Context {
    pub host: String,
    pub source: Source,
}

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
    /// Inside an RPC call that may replay (see [`RpcScope`]).
    static IN_RPC: RefCell<bool> = const { RefCell::new(false) };
}

/// Sets this thread's context until the guard drops.
pub struct ContextGuard(Option<Context>);

impl Drop for ContextGuard {
    fn drop(&mut self) {
        let prev = self.0.take();
        CONTEXT.with(|c| *c.borrow_mut() = prev);
    }
}

pub fn enter(ctx: Option<Context>) -> ContextGuard {
    ContextGuard(CONTEXT.with(|c| std::mem::replace(&mut *c.borrow_mut(), ctx)))
}

pub fn context() -> Option<Context> {
    CONTEXT.with(|c| c.borrow().clone())
}

/// What the policy knows. One per process ([`policy`]); tests make their own.
pub struct Policy {
    /// The shortest valid run seen per seed (a seed is a template: parent,
    /// miner tx, nonce...; every variant of a block shares it).
    seeds: Bounded<[u8; 32], u32>,
    /// Alternative blocks with a new template replayed per parent.
    alt_per_parent: Bounded<[u8; 32], u32>,
    sources: Bounded<String, Bucket>,
    rpc_global: Bucket,
    /// Restricted-RPC sources refused until a time, and how often they offended.
    penalty: Bounded<String, (f64, u32)>,
    rpc_replay: Bucket,
    /// Counters for logs and tests.
    pub skipped: u64,
    pub refused: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Replay,
    /// Not replayed (and not stored, not relayed, the sender not punished).
    Skip(&'static str),
}

/// An alternative block, as far as the policy cares.
pub struct AltBlock {
    pub id: [u8; 32],
    pub parent: [u8; 32],
    pub seed: Seed,
    pub frames: u32,
    /// Its height, and the height of our tip.
    pub height: u64,
    pub top: u64,
    /// The seed of our main-chain block at the same height, if any (so a
    /// variant of it is recognised after a restart, when [`Policy`] is empty).
    pub main_seed: Option<Seed>,
}

impl Policy {
    pub fn new(now: f64) -> Self {
        Policy {
            seeds: Bounded::new(SEEDS),
            alt_per_parent: Bounded::new(PARENTS),
            sources: Bounded::new(SOURCES),
            rpc_global: Bucket::new(RPC_GLOBAL_FRAMES, RPC_GLOBAL_FRAMES_PER_MIN, now),
            penalty: Bounded::new(SOURCES),
            rpc_replay: Bucket::new(RPC_REPLAY_SECONDS, RPC_REPLAY_SECONDS_PER_MIN, now),
            skipped: 0,
            refused: 0,
        }
    }

    /// A run verified valid: its seed is now a known template.
    pub fn note_valid(&mut self, seed: &Seed, frames: u32) {
        let best = self.seeds.entry_or(seed.0, || frames);
        *best = (*best).min(frames);
    }

    pub fn known_seed(&self, seed: &Seed) -> Option<u32> {
        self.seeds.get(&seed.0).copied()
    }

    /// Whether to replay alternative block `b` arriving in context `ctx`.
    pub fn admit_alt(&mut self, b: &AltBlock, ctx: Option<&Context>) -> Decision {
        // Above our tip: it may reorganise the chain (its alternative parent
        // was admitted by these rules, so this costs real blocks).
        if b.height > b.top {
            return Decision::Replay;
        }
        // In a sync span that goes above our tip: a longer chain is being
        // offered, and this block is part of it. (A peer that lies about the
        // rest pays with the first invalid block: the daemon bans it.)
        if let Some(Context { source: Source::Sync { top }, .. }) = ctx {
            if *top > b.top {
                return Decision::Replay;
            }
        }
        // A variant or a re-stamped copy of a block we know (same seed: same
        // parent, same miner transaction): useless to fork choice unless a
        // longer chain is built on it.
        if self.known_seed(&b.seed).is_some() || b.main_seed.is_some_and(|s| s == b.seed) {
            self.skipped += 1;
            return Decision::Skip("a variant of a block this node has, at or below its tip");
        }
        let n = self.alt_per_parent.entry_or(b.parent, || 0);
        if *n >= ALT_PER_PARENT {
            self.skipped += 1;
            return Decision::Skip("enough alternative blocks on this parent already");
        }
        *n += 1;
        Decision::Replay
    }

    /// Charges a block that arrived unasked (`ctx` Relay or RpcSubmit) its
    /// claimed `frames`; Skip when its source (or restricted RPC as a whole)
    /// has used up its budget, or is serving a penalty.
    pub fn admit_relay(&mut self, frames: u32, ctx: Option<&Context>, now: f64) -> Decision {
        let Some(ctx) = ctx else { return Decision::Replay };
        let f = frames.max(1) as f64;
        match ctx.source {
            Source::Sync { .. } => Decision::Replay,
            Source::Relay => {
                let b = self.sources.entry_or(format!("p2p {}", ctx.host), || Bucket::new(SOURCE_FRAMES, SOURCE_FRAMES_PER_MIN, now));
                if b.take(f, now) {
                    Decision::Replay
                } else {
                    self.refused += 1;
                    Decision::Skip("this peer's replay budget is used up")
                }
            }
            Source::RpcSubmit => {
                if let Some((until, _)) = self.penalty.get(&ctx.host) {
                    if now < *until {
                        self.refused += 1;
                        return Decision::Skip("this address submitted an invalid proof recently");
                    }
                }
                let b = self.sources.entry_or(format!("rpc {}", ctx.host), || Bucket::new(SOURCE_FRAMES, SOURCE_FRAMES_PER_MIN, now));
                if !b.take(f, now) {
                    self.refused += 1;
                    return Decision::Skip("this address's replay budget is used up");
                }
                if !self.rpc_global.take(f, now) {
                    self.refused += 1;
                    return Decision::Skip("restricted RPC's replay budget is used up");
                }
                Decision::Replay
            }
        }
    }

    /// Whether a restricted-RPC source may submit at all right now (before
    /// the block is even parsed further).
    pub fn rpc_submit_allowed(&mut self, host: &str, now: f64) -> bool {
        !matches!(self.penalty.get(&host.to_string()), Some((until, _)) if now < *until)
    }

    /// A replay in context `ctx` found the run invalid.
    pub fn note_invalid(&mut self, ctx: Option<&Context>, now: f64) {
        if let Some(Context { host, source: Source::RpcSubmit }) = ctx {
            let (_, times) = self.penalty.get(host).copied().unwrap_or((0.0, 0));
            let secs = (RPC_PENALTY_SECONDS * f64::powi(2.0, times.min(8) as i32)).min(86400.0);
            self.penalty.insert(host.clone(), (now + secs, times + 1));
        }
    }

    /// May an RPC-caused replay start now?
    pub fn rpc_replay_ok(&mut self, now: f64) -> bool {
        self.rpc_replay.positive(now)
    }
    pub fn rpc_replay_charge(&mut self, seconds: f64, now: f64) {
        self.rpc_replay.charge(seconds, now);
    }
}

pub fn policy() -> &'static Mutex<Policy> {
    static P: OnceLock<Mutex<Policy>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(Policy::new(now())))
}

fn with<R>(f: impl FnOnce(&mut Policy) -> R) -> R {
    let mut p = policy().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut p)
}

/// [`Policy::admit_alt`] on the process's policy, in this thread's context;
/// an admitted block that came unasked is then charged like any other
/// ([`Policy::admit_relay`]).
pub fn admit_alt(b: &AltBlock) -> Decision {
    let ctx = context();
    let t = now();
    with(|p| match p.admit_alt(b, ctx.as_ref()) {
        Decision::Replay => p.admit_relay(b.frames, ctx.as_ref(), t),
        skip => skip,
    })
}

/// [`Policy::admit_relay`] on the process's policy, in this thread's context.
pub fn admit_relay(frames: u32) -> Decision {
    let ctx = context();
    with(|p| p.admit_relay(frames, ctx.as_ref(), now()))
}

pub fn rpc_submit_allowed(host: &str) -> bool {
    with(|p| p.rpc_submit_allowed(host, now()))
}

pub(crate) fn note_valid(seed: &Seed, frames: u32) {
    with(|p| p.note_valid(seed, frames))
}

pub(crate) fn note_invalid() {
    let ctx = context();
    if ctx.is_some() {
        with(|p| p.note_invalid(ctx.as_ref(), now()))
    }
}

/// Marks this thread as serving an RPC call until dropped: replays it causes
/// are timed and charged to the RPC replay budget, and refused once it is
/// used up ([`rpc_replay_gate`]).
pub struct RpcScope(bool);

impl RpcScope {
    pub fn enter() -> Self {
        RpcScope(IN_RPC.with(|r| std::mem::replace(&mut *r.borrow_mut(), true)))
    }
}

impl Drop for RpcScope {
    fn drop(&mut self) {
        let prev = self.0;
        IN_RPC.with(|r| *r.borrow_mut() = prev);
    }
}

/// Whether this thread is serving an RPC call ([`RpcScope`]).
pub fn in_rpc() -> bool {
    IN_RPC.with(|r| *r.borrow())
}

/// Called before every replay: outside an RPC call, None (go ahead); inside,
/// Err when the RPC replay budget is used up, else a timer to hand to
/// [`rpc_replay_done`].
pub(crate) fn rpc_replay_gate() -> std::result::Result<Option<f64>, String> {
    if !IN_RPC.with(|r| *r.borrow()) {
        return Ok(None);
    }
    let t = now();
    if with(|p| p.rpc_replay_ok(t)) {
        Ok(Some(t))
    } else {
        with(|p| p.refused += 1);
        Err("busy: this node's replay budget for RPC is used up, retry in a minute".into())
    }
}

pub(crate) fn rpc_replay_done(started: Option<f64>) {
    if let Some(t0) = started {
        let t = now();
        with(|p| p.rpc_replay_charge(t - t0, t));
    }
}

/// (skipped, refused) so far, for logs.
pub fn counters() -> (u64, u64) {
    with(|p| (p.skipped, p.refused))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alt(seed: u8, parent: u8, height: u64, top: u64) -> AltBlock {
        AltBlock { id: [seed ^ 0x55; 32], parent: [parent; 32], seed: Seed([seed; 32]), frames: 3000, height, top, main_seed: None }
    }
    fn relay(host: &str) -> Context {
        Context { host: host.into(), source: Source::Relay }
    }
    fn rpc(host: &str) -> Context {
        Context { host: host.into(), source: Source::RpcSubmit }
    }

    #[test]
    fn variants_of_known_blocks_are_not_replayed_unless_a_longer_chain_comes_with_them() {
        let mut p = Policy::new(0.0);
        p.note_valid(&Seed([7; 32]), 3000);
        // A variant (same seed) of a block at our tip's height: skipped, any frames.
        let v = alt(7, 1, 10, 10);
        assert!(matches!(p.admit_alt(&v, Some(&relay("a"))), Decision::Skip(_)));
        assert!(matches!(p.admit_alt(&AltBlock { frames: 10, ..alt(7, 1, 10, 10) }, None), Decision::Skip(_)));
        // Recognised by the main chain's seed at that height too (after a restart).
        let mut fresh = Policy::new(0.0);
        assert!(matches!(fresh.admit_alt(&AltBlock { main_seed: Some(Seed([7; 32])), ..alt(7, 1, 5, 10) }, None), Decision::Skip(_)));
        // In a sync span reaching above our tip: replayed (a longer chain is built on it).
        assert_eq!(p.admit_alt(&v, Some(&Context { host: "a".into(), source: Source::Sync { top: 11 } })), Decision::Replay);
        // A span that doesn't reach above the tip doesn't help a variant.
        assert!(matches!(p.admit_alt(&v, Some(&Context { host: "a".into(), source: Source::Sync { top: 10 } })), Decision::Skip(_)));
        // Above our tip: always (it may reorganise).
        assert_eq!(p.admit_alt(&alt(7, 1, 11, 10), None), Decision::Replay);
    }

    #[test]
    fn new_templates_get_a_few_replays_per_parent() {
        let mut p = Policy::new(0.0);
        for k in 0..ALT_PER_PARENT {
            assert_eq!(p.admit_alt(&alt(100 + k as u8, 1, 10, 10), None), Decision::Replay);
        }
        assert!(matches!(p.admit_alt(&alt(200, 1, 10, 10), None), Decision::Skip(_)));
        // Another parent has its own allowance (a deep honest fork: one per parent).
        assert_eq!(p.admit_alt(&alt(201, 2, 9, 10), None), Decision::Replay);
    }

    #[test]
    fn unasked_blocks_draw_on_their_sources_budget() {
        let mut p = Policy::new(0.0);
        let a = relay("10.0.0.1");
        let n = (SOURCE_FRAMES / 36000.0) as usize;
        for _ in 0..n {
            assert_eq!(p.admit_relay(36000, Some(&a), 0.0), Decision::Replay);
        }
        assert!(matches!(p.admit_relay(36000, Some(&a), 0.0), Decision::Skip(_)));
        // Another peer is unaffected; sync and internal blocks are never charged.
        assert_eq!(p.admit_relay(36000, Some(&relay("10.0.0.2")), 0.0), Decision::Replay);
        assert_eq!(p.admit_relay(36000, Some(&Context { host: "10.0.0.1".into(), source: Source::Sync { top: 1 } }), 0.0), Decision::Replay);
        assert_eq!(p.admit_relay(36000, None, 0.0), Decision::Replay);
        // A minute later the peer has one more longest block.
        assert_eq!(p.admit_relay(36000, Some(&a), 60.0), Decision::Replay);
        assert!(matches!(p.admit_relay(36000, Some(&a), 60.0), Decision::Skip(_)));
        // An honest peer relaying a 5000-frame block every 5 minutes never runs dry.
        let h = relay("10.0.0.3");
        for k in 0..100 {
            assert_eq!(p.admit_relay(5000, Some(&h), 300.0 * k as f64), Decision::Replay);
        }
    }

    #[test]
    fn an_invalid_rpc_submission_shuts_its_address_out_for_a_while() {
        let mut p = Policy::new(0.0);
        let a = rpc("1.2.3.4");
        assert_eq!(p.admit_relay(3000, Some(&a), 0.0), Decision::Replay);
        p.note_invalid(Some(&a), 1.0);
        assert!(!p.rpc_submit_allowed("1.2.3.4", 2.0));
        assert!(matches!(p.admit_relay(3000, Some(&a), 2.0), Decision::Skip(_)));
        assert!(p.rpc_submit_allowed("5.6.7.8", 2.0));
        assert!(p.rpc_submit_allowed("1.2.3.4", 1.0 + RPC_PENALTY_SECONDS + 1.0));
        // A repeat doubles it.
        p.note_invalid(Some(&a), 1000.0);
        assert!(!p.rpc_submit_allowed("1.2.3.4", 1000.0 + RPC_PENALTY_SECONDS + 1.0));
        assert!(p.rpc_submit_allowed("1.2.3.4", 1000.0 + 2.0 * RPC_PENALTY_SECONDS + 1.0));
        // P2P peers are left to the daemon's ban.
        p.note_invalid(Some(&relay("9.9.9.9")), 0.0);
        assert!(p.penalty.get(&"9.9.9.9".to_string()).is_none());
    }

    #[test]
    fn restricted_rpc_as_a_whole_has_a_budget() {
        let mut p = Policy::new(0.0);
        let mut ok = 0;
        // Many addresses (or one proxy's many users): the global budget holds.
        for k in 0..1000 {
            if p.admit_relay(36000, Some(&rpc(&format!("10.1.{}.{}", k / 256, k % 256))), 0.0) == Decision::Replay {
                ok += 1;
            }
        }
        assert_eq!(ok, (RPC_GLOBAL_FRAMES / 36000.0) as usize);
    }

    #[test]
    fn the_rpc_replay_budget_refuses_once_spent_and_refills() {
        let mut p = Policy::new(0.0);
        assert!(p.rpc_replay_ok(0.0));
        p.rpc_replay_charge(RPC_REPLAY_SECONDS + 5.0, 0.0);
        assert!(!p.rpc_replay_ok(1.0));
        // 5 s of debt at 0.2 s/s: 25 s.
        assert!(!p.rpc_replay_ok(24.0));
        assert!(p.rpc_replay_ok(26.0));
    }

    #[test]
    fn bounded_maps_forget_the_oldest() {
        let mut b: Bounded<u32, u32> = Bounded::new(3);
        for k in 0..10 {
            b.insert(k, k);
        }
        assert_eq!(b.len(), 3);
        assert!(b.get(&6).is_none() && b.get(&9).is_some());
    }

    #[test]
    fn the_thread_context_nests_and_restores() {
        assert!(context().is_none());
        {
            let _g = enter(Some(relay("x")));
            assert_eq!(context().unwrap().host, "x");
            {
                let _h = enter(Some(rpc("y")));
                assert_eq!(context().unwrap().source, Source::RpcSubmit);
            }
            assert_eq!(context().unwrap().host, "x");
        }
        assert!(context().is_none());
    }
}
