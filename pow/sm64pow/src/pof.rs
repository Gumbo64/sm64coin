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
//!   this node opens a window of [`POF_WINDOW_SECONDS`] (60 s, this node's
//!   clock). A sibling (same parent, so the same height) that becomes
//!   eligible inside the window, carries **strictly fewer frames** than our
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
        let (blocks, first) = self.maps();
        if blocks.get(&id).is_some() {
            return;
        }
        blocks.insert(id, Seen { parent, seed, frames, at: now, stale: stale(stamp, now), stamp });
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
        a.frames < t.frames && a.at <= opened.saturating_add(POF_WINDOW_SECONDS)
    }

    /// Whether an alternative block claiming `frames`, a sibling of our
    /// childless `tip`, arriving at `now` stamped `stamp`, could displace it
    /// (then it is worth a replay whatever admit.rs's per-parent cap says).
    pub fn candidate(&mut self, tip: &[u8; 32], frames: u32, stamp: u64, now: u64) -> bool {
        let (blocks, first) = self.maps();
        let Some(t) = blocks.get(tip).copied() else { return false };
        if t.stale && !stale(stamp, now) {
            return true;
        }
        let opened = first.get(&t.parent).copied().unwrap_or(t.at);
        !stale(stamp, now) && frames < t.frames && now <= opened.saturating_add(POF_WINDOW_SECONDS)
    }
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
        let mut p = Policy::default();
        let t0 = 1_000_000;
        p.note_eligible(id(10), P, id(100), 3000, t0, t0);
        // 30 s later, 2800 frames, another template: displaces.
        p.note_eligible(id(11), P, id(101), 2800, t0 + 30, t0 + 30);
        assert!(p.candidate(&id(10), 2800, t0 + 30, t0 + 30));
        assert!(p.displaces(&id(10), &id(11)));
        // Not longer or equal runs.
        p.note_eligible(id(12), P, id(102), 3000, t0 + 31, t0 + 31);
        assert!(!p.displaces(&id(10), &id(12)));
        assert!(!p.candidate(&id(10), 3000, t0 + 31, t0 + 31));
        // The window counts from the first eligible block on the parent, not from the current tip.
        p.note_eligible(id(13), P, id(103), 2700, t0 + 61, t0 + 61);
        assert!(!p.displaces(&id(11), &id(13)), "61 s after the first: the block we have stays");
        assert!(!p.candidate(&id(11), 2700, t0 + 61, t0 + 61));
        p.note_eligible(id(14), P, id(104), 2700, t0 + 60, t0 + 60);
        assert!(p.displaces(&id(11), &id(14)), "60 s: still inside");
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
