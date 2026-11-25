//! The hold policy (layer 1): when a node processes a block that is valid at
//! its own timestamp but not yet by the node's clock.
//!
//! Relay policy, not consensus: the daemon (`Blockchain::sm64_hold`) holds
//! such a block, instead of rejecting it, until the first second its run fits
//! the limit by the node's clock, then processes and relays it. So stamping a
//! block ahead of the clock gains nothing, and miners stamp "now". Pure
//! function of the rules ([`crate::rules::Rules::max_frames`]).
//!
//! Read next: proof.rs.

use crate::rules::active as rules;

/// The hold policy's decision (a node's relay policy, not consensus): when a
/// node may process a block extending its tip whose run is `frames` long, at
/// retarget `difficulty`, the parent timestamped `parent_ts`, the block
/// `block_ts`, the node's clock reading `now` (all unix seconds), and a
/// restart's `restart_delay` (the limit then is the limit that many seconds
/// earlier). `None`: process it now (it fits by the node's clock, or it is too
/// long even at its own timestamp and the normal path rejects it).
/// `Some(t)`: hold it until `t`, the first second the run fits by the node's
/// clock; stamping a block ahead of the clock therefore gains nothing.
///
/// Publish-on-find (v0.3): miners submit a run when they find it, stamped
/// with the second it becomes valid, up to [`MAX_HOLD_AHEAD_SECONDS`] ahead;
/// the node holds it *and relays it at once* (`Blockchain::sm64_hold`), so
/// every node sees pending runs, and processes it when it is eligible: when
/// it fits by the node's clock and its stamp is within the consensus future
/// limit ([`FUTURE_LIMIT_SECONDS`]). Stamped further ahead than
/// `MAX_HOLD_AHEAD_SECONDS`: `None` (the normal path rejects it).
pub fn hold_until(difficulty: u64, parent_ts: u64, block_ts: u64, now: u64, frames: u32, restart_delay: u64) -> Option<u64> {
    let r = rules();
    let limit_at = |t: u64| -> u32 {
        let gap = t.saturating_sub(parent_ts);
        if gap < restart_delay { 0 } else { r.max_frames(difficulty, gap - restart_delay) }
    };
    if frames > limit_at(block_ts) || block_ts > now.saturating_add(MAX_HOLD_AHEAD_SECONDS) {
        return None;
    }
    // Consensus accepts a stamp at most FUTURE_LIMIT_SECONDS ahead of the clock.
    let stamp_ok = block_ts.saturating_sub(FUTURE_LIMIT_SECONDS);
    let fits = if frames <= limit_at(now) {
        now
    } else {
        let (mut lo, mut hi) = (now, block_ts.max(now + 1));
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if frames <= limit_at(mid) { hi = mid } else { lo = mid }
        }
        hi
    };
    let due = fits.max(stamp_ok);
    if due <= now { None } else { Some(due) }
}

/// The consensus future-time limit (`CRYPTONOTE_BLOCK_FUTURE_TIME_LIMIT` in
/// the fork's cryptonote_config.h; must be equal).
pub const FUTURE_LIMIT_SECONDS: u64 = 120;
pub use crate::rules::MAX_HOLD_AHEAD_SECONDS;

#[cfg(test)]
mod hold_tests {
    use super::*;

    #[test]
    fn a_run_stamped_ahead_waits_for_the_nodes_clock() {
        let r = rules();
        let d = 1u64 << 12; // a difficulty whose limit is still growing
        let parent = 1_000_000u64;
        // Past the start offset (crate::rules::STAR_CYCLE.start_halvings), where the limit grows every second.
        let o = crate::rules::STAR_CYCLE.start_halvings * crate::rules::STAR_CYCLE.decay_halving;
        // A run that fits the limit o + 30 s after the parent, not before.
        let frames = r.max_frames(d, o + 30);
        assert!(frames > r.max_frames(d, o + 29));
        // Stamped o + 30 s after the parent but arriving at o + 10: held until the node's clock says o + 30.
        assert_eq!(hold_until(d, parent, parent + o + 30, parent + o + 10, frames, 0), Some(parent + o + 30));
        // Stamped further ahead: held until the stamp is within the consensus future limit.
        assert_eq!(hold_until(d, parent, parent + o + 150, parent + o + 10, frames, 0), Some(parent + o + 30));
        assert_eq!(hold_until(d, parent, parent + o + 400, parent + o + 10, frames, 0), Some(parent + o + 400 - FUTURE_LIMIT_SECONDS));
        // Publish-on-find: found long before it is valid, stamped at its valid time: held until then.
        let late = r.max_frames(d, o + 1000);
        let valid_at = (o..o + 2000).find(|&t| r.max_frames(d, t) >= late).unwrap();
        assert_eq!(hold_until(d, parent, parent + valid_at, parent + o, late, 0), Some(parent + valid_at));
        // Too far ahead to hold: the normal path rejects it.
        assert_eq!(hold_until(d, parent, parent + o + 10 + MAX_HOLD_AHEAD_SECONDS + 1, parent + o + 10, frames, 0), None);
        // Fits by the clock but stamped beyond the future limit: held until the stamp is acceptable.
        assert_eq!(hold_until(d, parent, parent + o + 300, parent + o + 100, frames, 0), Some(parent + o + 300 - FUTURE_LIMIT_SECONDS));
        // Arriving when it already fits: processed now.
        assert_eq!(hold_until(d, parent, parent + o + 30, parent + o + 31, frames, 0), None);
        // Too long even at its own timestamp: not held (the normal path rejects it).
        assert_eq!(hold_until(d, parent, parent + o + 20, parent + o + 10, frames, 0), None);
        // A restart: the limit is the limit `delay` seconds earlier.
        assert_eq!(hold_until(d, parent, parent + 3600 + o + 100, parent + 3600 + o + 10, frames, 3600), Some(parent + 3600 + o + 30));
        // At difficulty 1 the limit grows too (the start offset): a 2-minute run waits until it fits.
        let two_min = 2 * 60 * 30;
        let due = hold_until(1, parent, parent + o, parent, two_min, 0).expect("held at difficulty 1");
        let fit = (0..o).find(|&t| r.max_frames(1, t) >= two_min).unwrap();
        assert_eq!(due, (parent + fit).max(parent + o - FUTURE_LIMIT_SECONDS), "until it fits and its stamp is within the future limit");
    }
}
