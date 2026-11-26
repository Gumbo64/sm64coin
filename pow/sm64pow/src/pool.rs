//! The proof pool (v0.4 stage B, node policy, layer 5): the records for the height of our tip,
//! collected from gossip and from displaced blocks, ready for the next template
//! (docs/V0.4-DESIGN.md section 6).
//!
//! Our tip is block h. Its pool holds proofs for height h (templates on B_{h-1}) that the NEXT
//! block may include. Offers are judged cheap-first (parse, facts, height and parent, key, run,
//! window, signature, start), then replayed once (the cache makes the including block's replay a
//! hit). Replay budgets per parent: each source (peer host) at most [`PER_SOURCE_REPLAYS`];
//! replays that found no valid record at most `2 * cap` in all, after which only sources that
//! delivered a valid record before (and the local node) are replayed, so sybil peers with garbage
//! can't spend the budget honest relays need; at most `4 * cap` replays in all. An invalid or
//! bad-witness record means the caller drops its sender. Records are stored with their proof in the smallest stateless encoding.
//! The last [`KEEP_TIPS`] tips' pools are kept (a short reorg loses nothing), and the last
//! [`KEEP_SELECTIONS`] templates' fields are remembered by root, because miners get templates
//! without the field and the daemon reattaches it at submit.
//!
//! Policy, not consensus: the sizes and budgets here are free to change; the one consensus
//! switch it reads is `included_proofs` (off: nothing is offered, nothing is selected).
//!
//! Read next: ffi.rs (`sm64pow_pool_*`).

use crate::chain::Parent;
use crate::included::{Judge, Judged, Parent1, Real};
use crate::records::{self, Facts, Record, Window, Winner};
use crate::sign::cn_fast_hash;
use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

pub const KEEP_TIPS: usize = 8;
pub const KEEP_SELECTIONS: usize = 32;
/// Replays per parent that found no valid record, as a multiple of the cap (design section 6.2).
pub const REPLAY_BUDGET_PER_CAP: usize = 2;
/// Replays per parent in all, as a multiple of the cap (a valid record is a real search).
pub const MAX_REPLAYS_PER_CAP: usize = 4;
/// Replays per parent for one source (a peer's host; the local node is not limited).
pub const PER_SOURCE_REPLAYS: usize = 16;
/// Sources remembered as having delivered a valid record.
const VOUCHED: usize = 1024;
/// Records waiting to be relayed, at most.
const MAX_RELAY: usize = 512;

/// What an offer came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offer {
    /// New and valid: stored, queued for relay.
    New,
    /// Known, not for our tip, outside the window or the budget: ignore it.
    Ignored,
    /// Invalid: drop the sender.
    Invalid(String),
    /// Bad witness: drop the sender.
    BadWitness(String),
    /// The parent's end state isn't cached.
    NeedParent,
}

type Order = (u32, [u8; 32]);

#[derive(Clone)]
struct Entry {
    rec: Record,
    leaf: [u8; 32],
    order: Order,
}

struct TipPool {
    id: [u8; 32],
    winner: Winner,
    parent: Parent,
    entries: Vec<Entry>, // ascending by order
    keys: HashSet<[u8; 32]>,
    runs: HashSet<[u8; 32]>,
    replays: usize,
    /// Replays that found no valid record.
    failed: usize,
    per_source: std::collections::HashMap<String, usize>,
}

pub struct Pool {
    window: Window,
    tips: VecDeque<TipPool>,
    /// Our current tip is the last of `tips`; `relay` holds new records (wire bytes) for it.
    relay: VecDeque<Vec<u8>>,
    selections: VecDeque<([u8; 32], Vec<u8>)>,
    /// Sources (hosts) that delivered a valid record.
    vouched: crate::admit::Bounded<String, ()>,
}

/// A record that passed the cheap checks, with what the replay and the insert need.
pub struct Admitted {
    tip: [u8; 32],
    parent: Parent,
    rec: Record,
    facts: Facts,
    leaf: [u8; 32],
    order: Order,
    /// The offering source's host ("" for the local node).
    src: String,
}

impl Pool {
    pub fn new(window: Window) -> Pool {
        Pool { window, tips: VecDeque::new(), relay: VecDeque::new(), selections: VecDeque::new(), vouched: crate::admit::Bounded::new(VOUCHED) }
    }

    /// Our new tip is block h (its parent B_{h-1} in `h`): the pool collects proofs for h. A tip
    /// seen before keeps its pool (a reorg back).
    pub fn tip(&mut self, id: [u8; 32], h: &Parent1) -> Result<(), String> {
        let winner = h.winner()?;
        let parent = h.parent();
        if let Some(i) = self.tips.iter().position(|t| t.id == id) {
            let t = self.tips.remove(i).unwrap();
            self.tips.push_back(t);
        } else {
            self.tips.push_back(TipPool { id, winner, parent, entries: vec![], keys: HashSet::new(), runs: HashSet::new(), replays: 0, failed: 0, per_source: Default::default() });
            while self.tips.len() > KEEP_TIPS {
                self.tips.pop_front();
            }
            self.relay.clear(); // what waited was for another tip (the daemon re-announces its tip often: only a new one clears)
        }
        Ok(())
    }

    fn current(&mut self) -> Option<&mut TipPool> {
        self.tips.back_mut()
    }

    /// The cheap checks (no replay). Ok(Admitted): replay it, then [`Pool::insert`].
    pub fn admit(&mut self, bytes: &[u8], judge: &dyn Judge) -> Result<Admitted, Offer> {
        if !crate::features::active().included_proofs {
            return Err(ignored("included proofs are off"));
        }
        let window = self.window;
        let src = crate::admit::context().map(|c| c.host).unwrap_or_default();
        let vouched = src.is_empty() || self.vouched.get(&src).is_some();
        let Some(tip) = self.tips.back_mut() else { return Err(ignored("no tip yet")) };
        let rec = records::decode_one(bytes).map_err(|e| Offer::Invalid(e.to_string()))?;
        let facts = rec.facts().map_err(|e| Offer::Invalid(e.to_string()))?;
        let w = &tip.winner;
        if facts.height != w.height || facts.prev_id != w.prev_id {
            return Err(ignored("not for our tip (another height or parent)")); // another height or parent: not for our tip
        }
        if facts.key == w.key || facts.run.input_hash == w.input_hash {
            return Err(ignored("the tip's own key or run")); // the tip's own key or run
        }
        if tip.keys.contains(&facts.key) || tip.runs.contains(&facts.run.input_hash) {
            return Err(ignored("known (one proof per key, one run once)")); // one proof per key, one run once; the first stays
        }
        let archive = records::window_frames(w.difficulty, w.frames, window.archive_seconds);
        if facts.run.frames == 0 || facts.run.frames > archive || facts.run.frames > crate::rules::active().max_frames_any() {
            return Err(ignored("outside the proof window")); // outside our window (may be inside another tip's)
        }
        let order = (facts.run.frames, cn_fast_hash(&facts.seed));
        if tip.entries.len() >= window.cap && order > tip.entries[window.cap - 1].order {
            return Err(ignored("worse than the best cap records")); // the best `cap` would never include it
        }
        if !crate::sign::verify(&facts.key, &crate::sign::message(&facts.seed, &facts.run.input_hash, facts.run.frames), &rec.sig) {
            return Err(Offer::Invalid("the signature does not verify with the template's key".into()));
        }
        match judge.start(&tip.parent) {
            None => return Err(Offer::NeedParent),
            Some(s) if s != facts.run.start => return Err(Offer::Invalid("a proof starts where the height starts".into())),
            Some(_) => {}
        }
        if tip.replays >= MAX_REPLAYS_PER_CAP * window.cap {
            return Err(ignored("the replay budget for this parent is spent"));
        }
        if tip.failed >= REPLAY_BUDGET_PER_CAP * window.cap && !vouched {
            return Err(ignored("the replay budget for this parent is spent (for sources without a valid record)"));
        }
        let mine = tip.per_source.get(&src).copied().unwrap_or(0);
        if !src.is_empty() && mine >= PER_SOURCE_REPLAYS {
            return Err(ignored("this source's replay budget for this parent is spent"));
        }
        if !src.is_empty() {
            if tip.per_source.len() >= 4096 && mine == 0 {
                return Err(ignored("too many sources for this parent"));
            }
            tip.per_source.insert(src.clone(), mine + 1);
        }
        tip.replays += 1;
        let leaf = facts.leaf(&rec.sig);
        Ok(Admitted { tip: tip.id, parent: tip.parent, rec, facts, leaf, order, src })
    }

    /// Stores a judged record (None: it is not valid, nothing is stored). The tip may have moved
    /// since [`Pool::admit`]: the record then goes to its own tip's pool, if still kept.
    pub fn insert(&mut self, a: Admitted, judged: Judged) -> Offer {
        if matches!(judged, Judged::Invalid(_) | Judged::BadWitness(_)) {
            if let Some(t) = self.tips.iter_mut().find(|t| t.id == a.tip) {
                t.failed += 1;
            }
        }
        let normalized = match judged {
            Judged::Valid(n) => n,
            Judged::Invalid(w) => return Offer::Invalid(w),
            Judged::BadWitness(w) => return Offer::BadWitness(w),
            Judged::NeedParent => return Offer::NeedParent,
        };
        if !a.src.is_empty() {
            self.vouched.insert(a.src.clone(), ());
        }
        let is_current = self.tips.back().is_some_and(|t| t.id == a.tip);
        let Some(tip) = self.tips.iter_mut().find(|t| t.id == a.tip) else { return Offer::Ignored };
        if tip.keys.contains(&a.facts.key) || tip.runs.contains(&a.facts.run.input_hash) {
            return Offer::Ignored; // a duplicate offered meanwhile
        }
        let mut rec = a.rec;
        if let Some(n) = normalized {
            rec.proof = n;
        }
        tip.keys.insert(a.facts.key);
        tip.runs.insert(a.facts.run.input_hash);
        let at = tip.entries.partition_point(|e| e.order < a.order);
        tip.entries.insert(at, Entry { rec: rec.clone(), leaf: a.leaf, order: a.order });
        if is_current {
            let mut b = Vec::new();
            rec.encode_into(&mut b);
            if self.relay.len() < MAX_RELAY {
                self.relay.push_back(b);
            }
        }
        Offer::New
    }

    /// The next record to gossip (not consumed until [`Pool::pop_relay`]).
    pub fn peek_relay(&self) -> Option<&[u8]> {
        self.relay.front().map(|v| v.as_slice())
    }

    pub fn pop_relay(&mut self) {
        self.relay.pop_front();
    }

    /// The field for a template on top of `tip_id`: the best `cap` records for its height, in the
    /// canonical order, and its root. Remembered by root.
    pub fn select(&mut self, tip_id: &[u8; 32]) -> Option<(Vec<u8>, [u8; 32])> {
        if !crate::features::active().included_proofs {
            return None;
        }
        let tip = self.tips.iter().find(|t| &t.id == tip_id)?;
        let mut recs = Vec::new();
        let mut leaves = Vec::new();
        let mut size = 8;
        for e in tip.entries.iter().take(self.window.cap) {
            size += e.rec.header.len() + e.rec.miner_tx.len() + e.rec.proof.len() + 64 + 32 * e.rec.branch.len() + 16;
            if size > records::MAX_FIELD {
                break;
            }
            recs.push(e.rec.clone());
            leaves.push(e.leaf);
        }
        if recs.is_empty() {
            return None;
        }
        let root = records::root(&leaves);
        let field = records::encode(&recs);
        if !self.selections.iter().any(|(r, _)| *r == root) {
            self.selections.push_back((root, field.clone()));
            while self.selections.len() > KEEP_SELECTIONS {
                self.selections.pop_front();
            }
        }
        Some((field, root))
    }

    pub fn field_for_root(&self, root: &[u8; 32]) -> Option<&[u8]> {
        self.selections.iter().find(|(r, _)| r == root).map(|(_, f)| f.as_slice())
    }

    /// Records held for the current tip (tests, status).
    pub fn len(&self) -> usize {
        self.tips.back().map_or(0, |t| t.entries.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Offers `bytes` to `pool`: the cheap checks under its lock, the replay outside it (a replay
/// takes seconds and templates must not wait for it), the insert under it again.
thread_local! {
    /// Why this thread's last offer was ignored (node logs; `sm64pow_pool_offer` reports it).
    pub static IGNORED_WHY: std::cell::Cell<&'static str> = const { std::cell::Cell::new("") };
}

fn ignored(why: &'static str) -> Offer {
    IGNORED_WHY.with(|c| c.set(why));
    Offer::Ignored
}

pub fn offer_with(pool: &Mutex<Pool>, bytes: &[u8], judge: &dyn Judge) -> anyhow::Result<Offer> {
    let a = match pool.lock().unwrap_or_else(|e| e.into_inner()).admit(bytes, judge) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    let judged = judge.replay_one(a.parent, &a.facts, &a.rec.proof)?;
    Ok(pool.lock().unwrap_or_else(|e| e.into_inner()).insert(a, judged))
}

/// The process-wide pool (the daemon has one chain).
pub fn global() -> &'static Mutex<Pool> {
    static POOL: std::sync::OnceLock<Mutex<Pool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| Mutex::new(Pool::new(records::PROPOSED)))
}

pub fn offer(bytes: &[u8]) -> anyhow::Result<Offer> {
    offer_with(global(), bytes, &Real)
}

/// A sealed block (a displaced tip, or any block we know) as a proof record: its template, its
/// committed proof and its signature. Err: it doesn't parse.
pub fn record_from_block(blob: &[u8]) -> Result<Record, &'static str> {
    let s = crate::template::split_sealed(blob)?;
    if s.proof.len() > records::MAX_PROOF {
        return Err("the block's proof is too large for a record");
    }
    Record::new(&s.parts, s.proof, s.sig)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::tests::{PREV, coinbase, header, record, run, winner};
    use crate::proof::Start;
    use crate::sign::Keypair;

    /// Replays are stand-ins: the verdict by key byte, no game.
    struct Fake {
        start: Option<Start>,
        replays: std::sync::atomic::AtomicUsize,
        bad: Mutex<Vec<[u8; 32]>>,
    }
    impl Fake {
        fn new() -> Fake {
            Fake { start: Some(Start::Parent), replays: 0.into(), bad: Mutex::new(vec![]) }
        }
        fn n(&self) -> usize {
            self.replays.load(std::sync::atomic::Ordering::Relaxed)
        }
    }
    impl Judge for Fake {
        fn start(&self, _: &Parent) -> Option<Start> {
            self.start
        }
        fn replay_all(&self, _: Parent, _: &[Facts], _: &[&[u8]], _: usize) -> anyhow::Result<crate::chain::RecordsVerdict> {
            Ok(crate::chain::RecordsVerdict::Valid)
        }
        fn replay_one(&self, _: Parent, f: &Facts, proof: &[u8]) -> anyhow::Result<Judged> {
            self.replays.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self.bad.lock().unwrap().contains(&f.key) {
                return Ok(Judged::Invalid("no progress".into()));
            }
            Ok(Judged::Valid(crate::included::smallest_stateless(proof)))
        }
    }

    fn kp(i: u8) -> Keypair {
        Keypair::from_entropy(&[i; 32]).unwrap()
    }

    /// The tip: block h = 5 on PREV, won by `winner()`'s key.
    fn tip_pool() -> (Pool, [u8; 32], Winner) {
        let (_, w) = winner();
        let mut p = Pool::new(records::PROPOSED);
        let id = [0x77; 32];
        let nonce = crate::sign::key_nonce(&w.key);
        let proof = crate::commit::commit_with(&crate::proof::RunLength, &crate::proof::Proof { frames: w.frames, start: Start::Parent, inputs: vec![sm64_binds::GamePad::new(9, 0, 0); w.frames as usize - 10] });
        let h = Parent1 { height: w.height, prev_id: PREV, prev_is_genesis: false, nonce: &nonce, proof: &proof, difficulty: w.difficulty };
        p.tip(id, &h).unwrap();
        let wf = h.winner().unwrap();
        assert_eq!((wf.frames, wf.key), (w.frames, w.key));
        (p, id, wf)
    }

    fn bytes(r: &Record) -> Vec<u8> {
        let mut b = vec![];
        r.encode_into(&mut b);
        b
    }

    fn rec(i: u8, frames: u32) -> Record {
        record(&kp(i), 5, &PREV, &run(frames, i as u16), 0).0
    }

    fn offer(p: &Mutex<Pool>, j: &Fake, r: &Record) -> Offer {
        offer_with(p, &bytes(r), j).unwrap()
    }

    #[test]
    fn dedupe_one_per_key_and_the_tips_own_key_and_run() {
        let (p, _, w) = tip_pool();
        let (p, j) = (Mutex::new(p), Fake::new());
        let a = rec(1, 2100);
        assert_eq!(offer(&p, &j, &a), Offer::New);
        assert_eq!(offer(&p, &j, &a), Offer::Ignored, "known");
        assert_eq!(j.n(), 1, "the repeat costs no replay");
        // the same key, another run: one proof per key
        let same_key = record(&kp(1), 5, &PREV, &run(2200, 77), 1).0;
        assert_eq!(offer(&p, &j, &same_key), Offer::Ignored);
        // the same run (input hash) from another key: one run counts once
        let dup_run = record(&kp(2), 5, &PREV, &run(2100, 1), 0).0;
        assert_eq!(offer(&p, &j, &dup_run), Offer::Ignored);
        // the tip's own key
        let own = record(&winner().0, 5, &PREV, &run(2100, 5), 0).0;
        assert_eq!(w.key, winner().0.public);
        assert_eq!(offer(&p, &j, &own), Offer::Ignored);
        assert_eq!(p.lock().unwrap().len(), 1);
        assert_eq!(j.n(), 1);
    }

    #[test]
    fn not_for_our_tip_and_the_window() {
        let (p, _, _) = tip_pool();
        let (p, j) = (Mutex::new(p), Fake::new());
        assert_eq!(offer(&p, &j, &record(&kp(1), 6, &PREV, &run(2100, 1), 0).0), Offer::Ignored, "another height");
        assert_eq!(offer(&p, &j, &record(&kp(2), 5, &[1; 32], &run(2100, 2), 0).0), Offer::Ignored, "another parent");
        let archive = records::window_frames(1 << 16, 2000, 60);
        assert!(archive > 2000 && archive < 3000);
        assert_eq!(offer(&p, &j, &rec(3, archive + 1)), Offer::Ignored, "outside the window");
        assert_eq!(offer(&p, &j, &rec(4, archive)), Offer::New, "the edge is inside");
        assert_eq!(offer(&p, &j, &rec(5, 1000)), Offer::New, "shorter than the winner is fine");
        assert_eq!(j.n(), 2, "ignored records cost no replay");
        // garbage
        assert!(matches!(offer_with(&p, &[1, 2, 3], &j).unwrap(), Offer::Invalid(_)));
        // a bad signature
        let mut bad = rec(6, 2100);
        bad.sig[0] ^= 1;
        assert!(matches!(offer(&p, &j, &bad), Offer::Invalid(_)));
    }

    #[test]
    fn start_state_and_judgements() {
        let (p, _, _) = tip_pool();
        let p = Mutex::new(p);
        let mut j = Fake::new();
        j.start = None;
        assert_eq!(offer(&p, &j, &rec(1, 2100)), Offer::NeedParent);
        j.start = Some(Start::PowerOn);
        assert!(matches!(offer(&p, &j, &rec(1, 2100)), Offer::Invalid(_)), "starts where the height starts");
        j.start = Some(Start::Parent);
        j.bad.lock().unwrap().push(kp(2).public);
        assert!(matches!(offer(&p, &j, &rec(2, 2100)), Offer::Invalid(_)), "an invalid run: drop the sender");
        assert_eq!(p.lock().unwrap().len(), 0, "nothing stored");
        assert_eq!(offer(&p, &j, &rec(1, 2100)), Offer::New);
    }

    #[test]
    fn selection_is_canonical_best_cap_and_remembered_by_root() {
        let (p, id, w) = tip_pool();
        let (p, j) = (Mutex::new(p), Fake::new());
        // 70 distinct keys, frames scrambled within the window
        for i in 0..70u32 {
            let frames = 2000 + (i * 37) % 200 + i;
            let r = record(&kp(i as u8 + 1), 5, &PREV, &run(frames, i as u16 + 1), 0).0;
            let o = offer(&p, &j, &r);
            assert!(matches!(o, Offer::New | Offer::Ignored), "{o:?}");
        }
        let (field, root) = p.lock().unwrap().select(&id).unwrap();
        let recs = records::decode(&field).unwrap();
        assert_eq!(recs.len(), 64, "the best cap");
        let (facts, _) = records::check(&recs, Some(&root), &w, Start::Parent, &records::PROPOSED).expect("canonical order, root, rules: records::check accepts it");
        assert_eq!(facts.len(), 64);
        // every shorter record than the worst selected is in; nothing selected is longer than a left-out one
        let worst = facts.iter().map(|f| (f.run.frames, cn_fast_hash(&f.seed))).max().unwrap();
        let all: Vec<_> = p.lock().unwrap().tips.back().unwrap().entries.iter().map(|e| e.order).collect();
        assert!(all.iter().filter(|o| **o <= worst).count() == 64);
        // root memory
        assert_eq!(p.lock().unwrap().field_for_root(&root), Some(field.as_slice()));
        assert_eq!(p.lock().unwrap().field_for_root(&[9; 32]), None);
        // unknown tip: nothing
        assert!(p.lock().unwrap().select(&[1; 32]).is_none());
    }

    #[test]
    fn budget_is_four_times_the_cap_per_parent_and_worse_than_the_cap_is_skipped_unreplayed() {
        let (p, _, _) = tip_pool();
        let (p, j) = (Mutex::new(p), Fake::new());
        // fill with 64 long records, then offer shorter ones: each is replayed until the budget
        for i in 0..64u8 {
            assert_eq!(offer(&p, &j, &record(&kp(i + 1), 5, &PREV, &run(2250, 1000 + i as u16), 0).0), Offer::New);
        }
        assert_eq!(j.n(), 64);
        assert_eq!(offer(&p, &j, &record(&kp(100), 5, &PREV, &run(2330, 1), 0).0), Offer::Ignored, "worse than the 64th: no replay");
        assert_eq!(j.n(), 64);
        let mut news = 0;
        let kp2 = |i: u16| {
            let mut e = [3u8; 32];
            e[..2].copy_from_slice(&i.to_le_bytes());
            Keypair::from_entropy(&e).unwrap()
        };
        for i in 0..200u16 {
            if offer(&p, &j, &record(&kp2(i), 5, &PREV, &run(1500 - i as u32, 3000 + i), 0).0) == Offer::New {
                news += 1;
            }
        }
        assert_eq!(news, 192, "256 replays in all (each one shorter, so worth trying)");
        assert_eq!(j.n(), 256);
        assert_eq!(p.lock().unwrap().len(), 256);
    }

    #[test]
    fn per_source_budgets_and_failed_replays_spare_vouched_sources() {
        use crate::admit::{Context, Source, enter};
        let (p, _, _) = tip_pool();
        let (p, j) = (Mutex::new(p), Fake::new());
        let from = |host: &str| enter(Some(Context { host: host.into(), source: Source::Relay }));
        let mut k = 0u8;
        let mut next = |frames: u32| {
            k += 1;
            record(&kp(k), 5, &PREV, &run(frames, k as u16 + 500), 0).0
        };
        // An honest relay delivers a valid record: vouched.
        {
            let _g = from("honest");
            assert_eq!(offer(&p, &j, &next(2100)), Offer::New);
        }
        // One source: at most PER_SOURCE_REPLAYS replays per parent.
        {
            let _g = from("chatty");
            for _ in 0..PER_SOURCE_REPLAYS {
                assert_eq!(offer(&p, &j, &next(2100)), Offer::New);
            }
            assert_eq!(offer(&p, &j, &next(2100)), Offer::Ignored, "this source's budget");
        }
        // Sybils with garbage (every replay invalid) spend the failure budget, 1 replay each.
        for i in 0..(REPLAY_BUDGET_PER_CAP * 64) {
            let r = next(2100);
            j.bad.lock().unwrap().push(r.facts().unwrap().key);
            let _g = from(&format!("sybil{i}"));
            assert!(matches!(offer(&p, &j, &r), Offer::Invalid(_)));
        }
        let before = j.n();
        {
            let _g = from("newcomer");
            assert_eq!(offer(&p, &j, &next(2100)), Offer::Ignored, "no valid record yet: not replayed once the failure budget is spent");
        }
        assert_eq!(j.n(), before);
        {
            let _g = from("honest");
            assert_eq!(offer(&p, &j, &next(2100)), Offer::New, "a vouched source still is");
        }
        assert_eq!(offer(&p, &j, &next(2100)), Offer::New, "and the local node");
    }

    #[test]
    fn relay_queue_and_tip_switching() {
        let (p, id, w) = tip_pool();
        let (p, j) = (Mutex::new(p), Fake::new());
        let a = rec(1, 2100);
        assert_eq!(offer(&p, &j, &a), Offer::New);
        assert_eq!(offer(&p, &j, &rec(2, 2101)), Offer::New);
        {
            let g = p.lock().unwrap();
            assert_eq!(g.peek_relay().unwrap(), bytes(&a).as_slice(), "relay in arrival order");
        }
        p.lock().unwrap().pop_relay();
        // a new tip (block h+1, height 6, parent = h): the old pool is kept, the queue cleared
        let nonce = crate::sign::key_nonce(&kp(50).public);
        let proof = crate::commit::commit_with(&crate::proof::RunLength, &crate::proof::Proof { frames: 2100, start: Start::Parent, inputs: vec![sm64_binds::GamePad::new(3, 0, 0); 2090] });
        let h2 = Parent1 { height: 6, prev_id: id, prev_is_genesis: false, nonce: &nonce, proof: &proof, difficulty: w.difficulty };
        p.lock().unwrap().tip([0x88; 32], &h2).unwrap();
        assert!(p.lock().unwrap().peek_relay().is_none());
        assert_eq!(p.lock().unwrap().len(), 0);
        assert_eq!(offer(&p, &j, &rec(1, 2100)), Offer::Ignored, "a height-5 record is not for the new tip");
        assert!(p.lock().unwrap().select(&id).is_some(), "the old tip's pool still builds its template");
        // reorg back: the pool is as it was
        let nonce0 = crate::sign::key_nonce(&winner().0.public);
        let proof0 = crate::commit::commit_with(&crate::proof::RunLength, &crate::proof::Proof { frames: w.frames, start: Start::Parent, inputs: vec![sm64_binds::GamePad::new(9, 0, 0); w.frames as usize - 10] });
        let h0 = Parent1 { height: 5, prev_id: PREV, prev_is_genesis: false, nonce: &nonce0, proof: &proof0, difficulty: w.difficulty };
        p.lock().unwrap().tip(id, &h0).unwrap();
        assert_eq!(p.lock().unwrap().len(), 2);
        // only KEEP_TIPS pools are kept
        for i in 0..KEEP_TIPS as u8 + 1 {
            p.lock().unwrap().tip([0xa0 + i; 32], &h2).unwrap();
        }
        assert!(p.lock().unwrap().select(&id).is_none());
    }

    #[test]
    fn selections_are_remembered_by_root_up_to_the_last_32() {
        let (p, id, _) = tip_pool();
        let (p, j) = (Mutex::new(p), Fake::new());
        let mut roots = vec![];
        for i in 0..34u8 {
            assert_eq!(offer(&p, &j, &rec(i + 1, 2000 + i as u32)), Offer::New);
            roots.push(p.lock().unwrap().select(&id).unwrap().1);
        }
        let g = p.lock().unwrap();
        assert!(g.field_for_root(&roots[0]).is_none() && g.field_for_root(&roots[1]).is_none());
        assert!(g.field_for_root(&roots[2]).is_some() && g.field_for_root(&roots[33]).is_some());
    }

    #[test]
    fn normalization_is_stateless_smallest_and_keeps_the_leaf() {
        use crate::proof::{Proof, Raw};
        let pr = Proof { frames: 2100, start: Start::Parent, inputs: vec![sm64_binds::GamePad::new(5, 0, 0); 2090] };
        let raw = crate::commit::commit_with(&Raw, &pr);
        let n = crate::included::smallest_stateless(&raw).unwrap();
        assert!(n.len() < raw.len() / 10);
        assert_eq!(crate::commit::header(&n).unwrap(), crate::commit::header(&raw).unwrap());
        assert!(crate::included::smallest_stateless(&[1, 2, 3]).is_none());
        let mut lie = raw.clone();
        *lie.last_mut().unwrap() ^= 0xff;
        assert!(crate::included::smallest_stateless(&lie).is_none(), "does not decode to its hash: not normalized");
    }

    #[test]
    fn a_sealed_block_is_a_record_that_matches_its_seed_key_and_proof() {
        let k = kp(9);
        let nonce = records::template_nonce(&k.public, Some(&crate::records::tests::payout(3)), None);
        let mtx = coinbase(5, &nonce);
        let parts = crate::template::TemplateParts { header: header(&PREV, 1_800_000_000, 7), miner_tx: mtx, tx_hashes: vec![[3; 32], [4; 32]] };
        let f = crate::template::facts(&parts).unwrap();
        let pr = run(2100, 4);
        let proof = crate::commit::commit_with(&crate::proof::RunLength, &pr);
        let sig = k.sign_run(&f.seed, &crate::commit::proof_id(&proof), 2100);
        let mut blob = crate::template::encode(&parts)[..0].to_vec();
        // the block blob: header, miner_tx, tx count, hashes, proof, sig
        blob.extend_from_slice(&parts.header);
        blob.extend_from_slice(&parts.miner_tx);
        blob.push(2);
        for h in &parts.tx_hashes {
            blob.extend_from_slice(h);
        }
        crate::records::tests::varint(&mut blob, proof.len() as u64);
        blob.extend_from_slice(&proof);
        blob.extend_from_slice(&sig);
        let r = record_from_block(&blob).unwrap();
        let rf = r.facts().unwrap();
        assert_eq!((rf.seed, rf.key, rf.height, rf.prev_id), (f.seed, k.public, 5, PREV));
        assert_eq!((rf.run.frames, rf.run.input_hash), (2100, crate::commit::proof_id(&proof)));
        assert_eq!(r.proof, proof);
        assert_eq!(r.leaf().unwrap(), records::leaf(&f.seed, &rf.run.input_hash, 2100, &sig));
        assert_eq!(r.tx_count, 3);
        assert!(crate::sign::verify(&rf.key, &crate::sign::message(&rf.seed, &rf.run.input_hash, 2100), &r.sig));
        // with a trailing sm64_proofs field after the signature
        let field = records::encode(&[rec(1, 2100)]);
        let mut with = blob.clone();
        crate::records::tests::varint(&mut with, field.len() as u64);
        with.extend_from_slice(&field);
        let s = crate::template::split_sealed(&with).unwrap();
        assert_eq!(s.field, field);
        assert_eq!(record_from_block(&with).unwrap(), r, "the field does not change the record");
        // an empty trailing field is fine; junk is not
        let mut empty = blob.clone();
        empty.push(0);
        assert_eq!(record_from_block(&empty).unwrap(), r);
        let mut junk = blob.clone();
        junk.extend_from_slice(&[5, 1]);
        assert!(record_from_block(&junk).is_err());
        for n in 0..blob.len() {
            assert!(record_from_block(&blob[..n]).is_err(), "truncated {n}");
        }
    }
}
