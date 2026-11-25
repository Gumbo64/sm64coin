//! Publish-on-find: the displacement window and the freshness bound (layer 5,
//! relay / fork-choice policy; nothing here changes validity).
//!
//! The variant FAIRNESS.md simulated (branch fairness-research,
//! pow/tools/auction/headstart/FAIRNESS.md sections 2, 6-9):
//!
//! - **Runs are relayed when found and eligible when valid.** A miner submits
//!   its run at once, stamped with the second it fits the limit; nodes hold
//!   it until then (hold.rs) and relay it immediately, so everyone sees the
//!   pending runs. The time limit is unchanged: a block is valid only if its
//!   run fits the limit at its stamp, and it is processed only when it fits
//!   by the node's clock. The auction's shortest-run selection stays: a
//!   shorter run is eligible sooner.
//! - **The window.** The first block on a parent that becomes eligible at
//!   this node opens a window of [`POF_WINDOW_SECONDS`] (30 s, this node's
//!   clock). A sibling (same parent, so the same height) that becomes
//!   eligible inside the window, carries **strictly fewer frames** (at equal
//!   frames: a lower cn_fast_hash(sm64_sig)) than our
//!   current block at that height, and has **another miner key** (another
//!   template: a different seed) displaces it: the node reorganises to it
//!   ([`Policy::displaces`]). After the window, or once our block has a
//!   child, the block we have stays (first seen, as before). Runs from the
//!   same key follow the equivocation rule instead (equivocation.rs).
//!   Honest miners keep searching the height until the window closes;
//!   nobody has built on a displaced block, so it costs no work.
//! - **The freshness bound** ([`FRESHNESS_SECONDS`], 60 s): a block stamped
//!   more than that before it arrived is stale. A stale block extending the
//!   tip is accepted (a longer chain always wins, so syncing nodes and
//!   partitions converge) but not relayed, and while it has no child a fresh
//!   sibling displaces it. That caps back-dating at Δ per block.
//!
//! How nodes agree: the comparison (same parent, other key, fewer frames,
//! freshness of the stamps) is deterministic; only "inside the window" reads
//! the node's own clock, so two nodes can disagree only about a sibling that
//! arrives within propagation delay of the window's end. That leaves at most
//! a one-block split, which the next block resolves by length, as any race
//! does. Signatures are what make this safe: without them a displacing run
//! could be ground from someone else's published run (a variant one frame
//! shorter), and the window would hand the block to the copier.
//!
//! Read next: chain.rs.

use crate::admit::Bounded;
pub use crate::rules::{FRESHNESS_SECONDS, POF_WINDOW_SECONDS};
use std::sync::{Mutex, OnceLock};

const BLOCKS: usize = 4096;

/// A block as this node first saw it eligible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seen {
    pub parent: [u8; 32],
    pub seed: [u8; 32],
    pub frames: u32,
    /// When it became eligible here (unix seconds, this node's clock).
    pub at: u64,
    /// Stamped more than [`FRESHNESS_SECONDS`] before it became eligible here.
    pub stale: bool,
    /// Its timestamp.
    pub stamp: u64,
    /// The equal-length tie-break ([`Tie`]: the canonical proof size, then cn_fast_hash(sm64_sig);
    /// lower wins; [`NO_TIE`] when unknown).
    pub tie: Tie,
}

/// Whether a block stamped `stamp` and arriving (eligible) at `now` is stale.
pub fn stale(stamp: u64, now: u64) -> bool {
    stamp.saturating_add(FRESHNESS_SECONDS) < now
}

#[derive(Default)]
pub struct Policy {
    blocks: Option<Bounded<[u8; 32], Seen>>,
    /// The window per parent: when its first child became eligible here.
    first: Option<Bounded<[u8; 32], u64>>,
}

impl Policy {
    fn maps(&mut self) -> (&mut Bounded<[u8; 32], Seen>, &mut Bounded<[u8; 32], u64>) {
        (self.blocks.get_or_insert_with(|| Bounded::new(BLOCKS)), self.first.get_or_insert_with(|| Bounded::new(BLOCKS)))
    }

    /// Block `id` (on `parent`, template `seed`, `frames` long, stamped
    /// `stamp`) is eligible at `now`: valid and fitting by this node's clock.
    /// The first time only.
    pub fn note_eligible(&mut self, id: [u8; 32], parent: [u8; 32], seed: [u8; 32], frames: u32, stamp: u64, now: u64) {
        self.note_eligible_tie(id, parent, seed, frames, stamp, now, NO_TIE)
    }

    /// [`Policy::note_eligible`] with the block's tie-break hash, `cn_fast_hash(sm64_sig)` ([`tie_of`]).
    #[allow(clippy::too_many_arguments)]
    pub fn note_eligible_tie(&mut self, id: [u8; 32], parent: [u8; 32], seed: [u8; 32], frames: u32, stamp: u64, now: u64, tie: Tie) {
        let (blocks, first) = self.maps();
        if blocks.get(&id).is_some() {
            return;
        }
        blocks.insert(id, Seen { parent, seed, frames, at: now, stale: stale(stamp, now), stamp, tie });
        if first.get(&parent).is_none() {
            first.insert(parent, now);
        }
    }

    pub fn seen(&mut self, id: &[u8; 32]) -> Option<Seen> {
        self.maps().0.get(id).copied()
    }

    /// Whether `alt` (eligible, a sibling of our childless block `tip`)
    /// replaces it.
    pub fn displaces(&mut self, tip: &[u8; 32], alt: &[u8; 32]) -> bool {
        let (blocks, first) = self.maps();
        let (Some(t), Some(a)) = (blocks.get(tip).copied(), blocks.get(alt).copied()) else { return false };
        if t.parent != a.parent {
            return false; // not siblings
        }
        if t.seed != a.seed && crate::equivocation::policy().lock().unwrap_or_else(|e| e.into_inner()).tainted(&a.seed) {
            return false; // the window never overrides the equivocation penalty (fork choice decides)
        }
        if t.seed == a.seed {
            // The same key: the same run restamped (a second different run is equivocation.rs's).
            let opened = first.get(&t.parent).copied().unwrap_or(t.at);
            return !a.stale && a.at <= opened.saturating_add(POF_WINDOW_SECONDS) && copy_preferred(&a.seed, (a.stamp, alt), (t.stamp, tip));
        }
        if t.stale && !a.stale {
            return true; // the freshness bound: a stale tip loses the tie
        }
        if a.stale {
            return false;
        }
        let opened = first.get(&t.parent).copied().unwrap_or(t.at);
        beats(a.frames, &a.tie, t.frames, &t.tie) && a.at <= opened.saturating_add(POF_WINDOW_SECONDS)
    }

    /// Whether an alternative block claiming `frames`, a sibling of our
    /// childless `tip`, arriving at `now` stamped `stamp`, could displace it
    /// (then it is worth a replay whatever admit.rs's per-parent cap says).
    pub fn candidate(&mut self, tip: &[u8; 32], frames: u32, stamp: u64, now: u64) -> bool {
        self.candidate_tie(tip, frames, &NO_TIE, stamp, now)
    }

    /// [`Policy::candidate`] for a block whose tie-break hash is `tie` (an equal-length sibling
    /// with a lower hash also displaces, inside the window).
    pub fn candidate_tie(&mut self, tip: &[u8; 32], frames: u32, tie: &Tie, stamp: u64, now: u64) -> bool {
        let (blocks, first) = self.maps();
        let Some(t) = blocks.get(tip).copied() else { return false };
        if t.stale && !stale(stamp, now) {
            return true;
        }
        let opened = first.get(&t.parent).copied().unwrap_or(t.at);
        !stale(stamp, now) && beats(frames, tie, t.frames, &t.tie) && now <= opened.saturating_add(POF_WINDOW_SECONDS)
    }
}

/// The equal-length tie-break between siblings of different keys: the smaller canonical proof
/// size wins ([`canonical_size`]: simpler, more compressible inputs, smaller blocks), then the
/// lower `cn_fast_hash(sm64_sig)`. Ordered by (size, hash).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Tie {
    pub size: u32,
    pub hash: [u8; 32],
}

/// "Unknown": never wins a tie.
pub const NO_TIE: Tie = Tie { size: u32::MAX, hash: [0xff; 32] };

/// A block's tie-break: its proof's [`canonical_size`] and `cn_fast_hash(sm64_sig)`.
pub fn tie_of(proof: &[u8], sig: &[u8; 64]) -> Tie {
    Tie { size: canonical_size(proof), hash: crate::sign::cn_fast_hash(sig) }
}

/// The size of a proof that every node sees the same for the same block id (v0.4: the id is the
/// committed input hash, so every encoding of the run must get one size): the length of the
/// run-length layout of the committed run (`varint(frames) || start || 1 || runs`, our own
/// deterministic codec). Known before the replay for a stateless encoding (decoded and
/// hash-checked here, cheap); for a state-coded blob only once some replay has decoded the run
/// ([`note_size`], kept by input hash), else u32::MAX (never wins a tie). docs/V0.4-DESIGN.md
/// section 1.6 (the reference encoder; statemix codec 5 is the owner's choice, run-length is
/// what the replay can measure today: a follow-up encodes codec 5 alongside the replay).
pub fn canonical_size(blob: &[u8]) -> u32 {
    let Ok(hdr) = crate::commit::header(blob) else { return u32::MAX };
    if let Ok(crate::commit::Opened::Run(_, pr)) = crate::commit::open(blob, crate::rules::active().max_frames_any(), false) {
        return crate::proof::encode_proof_with(&crate::proof::RunLength, &pr).len().min(u32::MAX as usize - 1) as u32;
    }
    sizes().lock().unwrap_or_else(|e| e.into_inner()).get(&hdr.input_hash).copied().unwrap_or(u32::MAX)
}

fn sizes() -> &'static Mutex<Bounded<[u8; 32], u32>> {
    static S: OnceLock<Mutex<Bounded<[u8; 32], u32>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Bounded::new(BLOCKS)))
}

/// A replay decoded the committed run of `hdr`: its canonical size is `size`.
pub fn note_size_of(input_hash: &[u8; 32], size: u32) {
    sizes().lock().unwrap_or_else(|e| e.into_inner()).insert(*input_hash, size);
}

/// [`note_size_of`] from a verified committed blob's layout (chain.rs): a stateless layout is
/// already the run-length one; a state-coded layout's size was noted by its replay.
pub(crate) fn note_size(hdr: &crate::commit::Header, layout: &[u8]) {
    if matches!(crate::proof::split_blob(layout), Ok((_, _, 1, _))) {
        note_size_of(&hdr.input_hash, layout.len().min(u32::MAX as usize - 1) as u32);
    }
}

/// Counts the run-length layout's size of a run as its inputs are decoded (replay.rs).
pub(crate) struct SizeCounter {
    size: usize,
    last: Option<[u8; 4]>,
    run: u64,
}

impl SizeCounter {
    pub(crate) fn new(frames: u32) -> Self {
        let mut v = Vec::new();
        crate::proof::put_varint(&mut v, frames as u64);
        SizeCounter { size: v.len() + 2, last: None, run: 0 }
    }
    fn flush(&mut self) {
        if self.run > 0 {
            let mut v = Vec::new();
            crate::proof::put_varint(&mut v, self.run);
            self.size += v.len() + 4;
        }
    }
    pub(crate) fn pad(&mut self, p: &sm64_binds::GamePad) {
        let b = crate::proof::pad_bytes(p);
        if self.last == Some(b) {
            self.run += 1;
        } else {
            self.flush();
            self.last = Some(b);
            self.run = 1;
        }
    }
    pub(crate) fn finish(mut self) -> u32 {
        self.flush();
        self.size.min(u32::MAX as usize - 1) as u32
    }
}

/// Inside the window, between siblings of different keys: strictly fewer frames wins; at equal
/// frames, the smaller canonical proof size, then the lower signature hash, at every node whatever
/// the arrival order (the auction-smoothness simulation: window-edge splits 0.8 % -> 0). Only the
/// run's owner can change its inputs or signature; grinding them is real optimisation (simpler
/// inputs) or only matters for exact ties.
fn beats(frames: u32, tie: &Tie, their_frames: u32, their_tie: &Tie) -> bool {
    frames < their_frames || (frames == their_frames && *tie != NO_TIE && tie < their_tie)
}

/// Copies of one run (the same seed and run, another timestamp: anyone can
/// restamp a published block, the signature doesn't cover the stamp) are one
/// block with a deterministic choice: the earliest stamp, then the lowest
/// block id. A node holding a later copy switches to the preferred one inside
/// the window (like a shorter run of another key), so a third party's
/// restamp can't split the network: every node ends on the same copy. Not
/// when the template has equivocation evidence (two different runs: then
/// equivocation.rs's rule applies instead).
fn copy_preferred(seed: &[u8; 32], a: (u64, &[u8; 32]), t: (u64, &[u8; 32])) -> bool {
    a < t && crate::equivocation::policy().lock().unwrap_or_else(|e| e.into_inner()).evidence(seed).is_none()
}

/// Whether a copy of our block `tip`'s run (same `seed`), stamped `stamp`
/// with id `id`, eligible at `at`, is preferred to it ([`copy_preferred`],
/// inside the window): worth a replay (a cached one: copies share the run's
/// end state).
pub fn restamp_preferred(tip: &[u8; 32], seed: &[u8; 32], stamp: u64, id: &[u8; 32], at: u64) -> bool {
    let (t, opened) = {
        let mut p = policy().lock().unwrap_or_else(|e| e.into_inner());
        let (blocks, first) = p.maps();
        let Some(t) = blocks.get(tip).copied() else { return false };
        (t, first.get(&t.parent).copied().unwrap_or(t.at))
    };
    t.seed == *seed && !stale(stamp, at) && at <= opened.saturating_add(POF_WINDOW_SECONDS) && copy_preferred(seed, (stamp, id), (t.stamp, tip))
}

/// Whether a sibling of our block `tip` (seed `seed`, claiming `frames`, stamped `stamp`,
/// eligible at `at`) could replace it: a [`Policy::candidate`] (shorter inside the window, or
/// fresh against a stale tip), or a clean sibling of a tip whose template has equivocation
/// evidence (equivocation.rs: miners build such siblings, and the fork-choice penalty prefers
/// them, so they must be replayed and stored, not dropped).
pub fn sibling_could_matter(tip: &[u8; 32], seed: &[u8; 32], frames: u32, tie: &Tie, stamp: u64, at: u64) -> bool {
    let (cand, tip_seed) = {
        let mut p = policy().lock().unwrap_or_else(|e| e.into_inner());
        let c = p.candidate_tie(tip, frames, tie, stamp, at);
        (c, p.seen(tip).map(|s| s.seed))
    };
    if cand {
        return true;
    }
    let Some(ts) = tip_seed else { return false };
    let e = crate::equivocation::policy().lock().unwrap_or_else(|e| e.into_inner());
    ts != *seed && e.avoid(&ts) && !e.avoid(seed)
}

pub fn policy() -> &'static Mutex<Policy> {
    static P: OnceLock<Mutex<Policy>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(Policy::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: [u8; 32] = [1; 32];

    fn id(b: u8) -> [u8; 32] {
        [b; 32]
    }

    #[test]
    fn a_shorter_run_of_another_key_displaces_inside_the_window_only() {
        const W: u64 = POF_WINDOW_SECONDS;
        let mut p = Policy::default();
        let t0 = 1_000_000;
        p.note_eligible(id(10), P, id(100), 3000, t0, t0);
        // Inside the window, 2800 frames, another template: displaces.
        p.note_eligible(id(11), P, id(101), 2800, t0 + W / 2, t0 + W / 2);
        assert!(p.candidate(&id(10), 2800, t0 + W / 2, t0 + W / 2));
        assert!(p.displaces(&id(10), &id(11)));
        // Not longer or equal runs (without a tie-break hash).
        p.note_eligible(id(12), P, id(102), 3000, t0 + W / 2 + 1, t0 + W / 2 + 1);
        assert!(!p.displaces(&id(10), &id(12)));
        assert!(!p.candidate(&id(10), 3000, t0 + W / 2 + 1, t0 + W / 2 + 1));
        // The window counts from the first eligible block on the parent, not from the current tip.
        p.note_eligible(id(13), P, id(103), 2700, t0 + W + 1, t0 + W + 1);
        assert!(!p.displaces(&id(11), &id(13)), "just after the window: the block we have stays");
        assert!(!p.candidate(&id(11), 2700, t0 + W + 1, t0 + W + 1));
        p.note_eligible(id(14), P, id(104), 2700, t0 + W, t0 + W);
        assert!(p.displaces(&id(11), &id(14)), "at the window's end: still inside");
    }

    #[test]
    fn equal_lengths_of_different_keys_go_to_the_lower_signature_hash_whatever_the_order() {
        let t0 = 3_000_000;
        let (lo, hi) = (Tie { size: 100, hash: [1u8; 32] }, Tie { size: 100, hash: [2u8; 32] });
        for first_low in [true, false] {
            let mut p = Policy::default();
            let (a, b) = if first_low { ((id(40), lo), (id(41), hi)) } else { ((id(41), hi), (id(40), lo)) };
            p.note_eligible_tie(a.0, P, [a.0[0] + 100; 32], 3000, t0, t0, a.1);
            p.note_eligible_tie(b.0, P, [b.0[0] + 100; 32], 3000, t0 + 5, t0 + 5, b.1);
            // Whichever arrived first, the lower hash is where every node ends.
            assert_eq!(p.displaces(&a.0, &b.0), !first_low, "first_low {first_low}");
            assert_eq!(p.candidate_tie(&a.0, 3000, &b.1, t0 + 5, t0 + 5), !first_low);
            assert!(!p.displaces(&id(40), &id(41)), "the higher hash never displaces the lower");
        }
        // Outside the window, or unknown hashes: no tie-break.
        let mut q = Policy::default();
        q.note_eligible_tie(id(50), P, id(150), 3000, t0, t0, hi);
        q.note_eligible_tie(id(51), P, id(151), 3000, t0 + POF_WINDOW_SECONDS + 1, t0 + POF_WINDOW_SECONDS + 1, lo);
        assert!(!q.displaces(&id(50), &id(51)));
        assert!(!q.candidate(&id(50), 3000, t0 + 1, t0 + 1), "no tie-break hash: never wins a tie");
    }

    #[test]
    fn at_equal_frames_the_smaller_proof_wins_before_the_hash() {
        let t0 = 4_000_000;
        let mut p = Policy::default();
        let big_low_hash = Tie { size: 900, hash: [0; 32] };
        let small_high_hash = Tie { size: 800, hash: [0xee; 32] };
        p.note_eligible_tie(id(60), P, id(160), 3000, t0, t0, big_low_hash);
        p.note_eligible_tie(id(61), P, id(161), 3000, t0 + 3, t0 + 3, small_high_hash);
        assert!(p.displaces(&id(60), &id(61)), "smaller proof first, whatever the hash");
        assert!(!p.displaces(&id(61), &id(60)));
        assert!(p.candidate_tie(&id(60), 3000, &small_high_hash, t0 + 3, t0 + 3));
        // Fewer frames still beats any size.
        p.note_eligible_tie(id(62), P, id(162), 2999, t0 + 4, t0 + 4, Tie { size: 5000, hash: [0xff; 32] });
        assert!(p.displaces(&id(61), &id(62)));
    }

    #[test]
    fn the_canonical_size_does_not_depend_on_the_encoding() {
        use crate::commit;
        use crate::proof::{Proof, RunLength, Raw, Start, encode_proof_with};
        use crate::sm64_binds::GamePad;
        let mut inputs = vec![GamePad::default(); 900];
        for (i, x) in inputs.iter_mut().enumerate() {
            x.button = (i / 50) as u16;
        }
        let pr = Proof { frames: 900, start: Start::Parent, inputs };
        let raw = commit::commit_with(&Raw, &pr);
        let rle = commit::commit_with(&RunLength, &pr);
        assert_ne!(raw.len(), rle.len());
        assert_eq!(commit::proof_id(&raw), commit::proof_id(&rle), "one block id for both encodings");
        assert_eq!(canonical_size(&raw), canonical_size(&rle), "and one size");
        assert_eq!(canonical_size(&raw), encode_proof_with(&RunLength, &pr).len() as u32);
        assert_eq!(canonical_size(&raw), 94, "pinned: the run-length layout of this run is 94 bytes");
        assert_eq!(canonical_size(&[0xff, 0xff]), u32::MAX, "doesn't parse");
        // The incremental counter (what a state-coded replay measures) agrees.
        let mut c = SizeCounter::new(pr.frames);
        pr.inputs.iter().for_each(|p| c.pad(p));
        assert_eq!(c.finish(), 94);
        // A state-coded blob: unknown until a replay noted its run.
        let st = commit::join(&commit::header_of(&Proof { frames: 901, ..pr.clone() }), &[4, 1, 2, 3]);
        assert_eq!(canonical_size(&st), u32::MAX);
        note_size_of(&commit::header(&st).unwrap().input_hash, 77);
        assert_eq!(canonical_size(&st), 77);
    }

    #[test]
    fn the_same_key_or_another_parent_never_displaces() {
        let mut p = Policy::default();
        p.note_eligible(id(10), P, id(100), 3000, 50, 50);
        p.note_eligible(id(11), P, id(100), 2000, 55, 55); // same seed = same key: equivocation.rs
        assert!(!p.displaces(&id(10), &id(11)));
        p.note_eligible(id(12), id(2), id(101), 2000, 55, 55);
        assert!(!p.displaces(&id(10), &id(12)));
        assert!(!p.displaces(&id(10), &id(99)), "unknown block");
    }

    #[test]
    fn restamped_copies_converge_on_the_earliest_stamp_inside_the_window() {
        let mut p = Policy::default();
        let t0 = 2_000_000;
        // B' (a third party's later restamp of B) arrived first; B (stamped earlier) displaces it, never the other way.
        p.note_eligible(id(21), P, id(200), 3000, t0 + 20, t0 + 20);
        p.note_eligible(id(20), P, id(200), 3000, t0, t0 + 21);
        assert!(p.displaces(&id(21), &id(20)), "the earlier stamp wins");
        assert!(!p.displaces(&id(20), &id(21)), "a later restamp never displaces");
        // Equal stamps: the lower id.
        p.note_eligible(id(23), P, id(201), 3000, t0, t0 + 5);
        p.note_eligible(id(22), P, id(201), 3000, t0, t0 + 6);
        assert!(p.displaces(&id(23), &id(22)) && !p.displaces(&id(22), &id(23)));
        // Outside the window: the block we have stays.
        let mut q = Policy::default();
        q.note_eligible(id(31), P, id(202), 3000, t0 + 20, t0 + 20);
        q.note_eligible(id(30), P, id(202), 3000, t0, t0 + 20 + POF_WINDOW_SECONDS + 1);
        assert!(!q.displaces(&id(31), &id(30)));
    }

    #[test]
    fn a_stale_tip_loses_the_tie_to_a_fresh_sibling() {
        let mut p = Policy::default();
        let now = 10_000;
        assert!(stale(now - FRESHNESS_SECONDS - 1, now) && !stale(now - FRESHNESS_SECONDS, now));
        // Back-dated by 10 minutes: stale.
        p.note_eligible(id(10), P, id(100), 2000, now - 600, now);
        // A fresh sibling, even a longer one, an hour later (while the tip has no child).
        p.note_eligible(id(11), P, id(101), 2500, now + 3600, now + 3600);
        assert!(p.candidate(&id(10), 2500, now + 3600, now + 3600));
        assert!(p.displaces(&id(10), &id(11)));
        // A stale sibling never displaces.
        p.note_eligible(id(12), P, id(102), 100, now - 600, now + 5);
        assert!(!p.displaces(&id(11), &id(12)));
    }
}
