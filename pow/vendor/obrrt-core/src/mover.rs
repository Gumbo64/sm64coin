//! The steering controller's hook: search/src/steer.rs asks the
//! registered [`Mover`] for every frame's inputs towards an objective's
//! target. The search knows nothing about what a mover does; the domain
//! knowledge (control modes, actions, move recipes, slopes) lives with
//! whoever registers one (obrrt-objectives' `moves`, which the run binary
//! always registers). Without one, steering plays a neutral pad.
//!
//! Determinism: read-only caches a mover consults (the static floor
//! snapshots, [`crate::geo::static_floors`]) fill in whenever some thread
//! first reads a state of a zone, so a mover that used them at once would
//! act differently depending on thread timing. A mover may only use zones
//! [`published`] by [`publish`], which the search calls between rounds
//! (when no rollout runs): what a round's rollouts see is then fixed by the
//! rounds before it.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock, RwLock};

use crate::{Arrival, GamePad, St};

/// The steering controller.
///
/// Everything must be a pure function of its arguments (and of read-only
/// caches such as the static floors): rollouts are replayed and compared,
/// so no hidden per-rollout state.
pub trait Mover: Send + Sync {
    /// The inputs for one frame of steering towards `target` from `st`.
    /// `arrival` is the objective's [`crate::Objective::arrival`] (a hint), `button`
    /// its [`crate::Objective::steer_button`] (0 if none); `frame` counts
    /// frames into the rollout; `jump_every` is a period for a generic A
    /// rhythm (the GA varies it); `policy` is one of [`Mover::policies`] to
    /// play where it applies (the GA's policy-seeded rollouts), else the
    /// default steering.
    #[allow(clippy::too_many_arguments)]
    fn control(&self, st: &St, target: [f32; 3], arrival: Arrival, button: u16, frame: usize, jump_every: usize, policy: Option<usize>) -> GamePad;

    /// How many forcible policies the mover offers (the GA's policy-seeded
    /// rollouts, `OBRRT_MOVE_GA`). Policy `p` in `0..policies()` is one
    /// fixed way of moving (e.g. "use move p whenever it can be used");
    /// what it means is the mover's business.
    fn policies(&self) -> usize {
        0
    }

    /// A short name for policy `p` (stats and logs).
    fn policy_name(&self, _p: usize) -> &str {
        "?"
    }

    /// A cheap check, before simulating, that policy `p` isn't obvious
    /// nonsense from `st` towards `target` (rollouts of implausible
    /// policies are skipped). Pure like [`Mover::pad`].
    fn policy_plausible(&self, _p: usize, _st: &St, _target: [f32; 3], _arrival: Arrival) -> bool {
        true
    }

    /// Whether `prev` -> `cur` starts a jump from the ground (the GA's
    /// `OBRRT_CALM` tie-break counts them).
    fn is_jump_start(&self, _prev: &St, _cur: &St) -> bool {
        false
    }

    /// Whether Mario is airborne or in a landing (diagnostics: door
    /// overshoots).
    fn in_flight(&self, _st: &St) -> bool {
        false
    }

    /// How useful jumping is from `st` towards `target`, as a multiplier
    /// (0..=1) on the A presses of random genomes (`OBRRT_CALM`).
    fn jump_useful(&self, _st: &St, _target: [f32; 3], _arrival: Arrival) -> f32 {
        1.0
    }
}

static MOVER: OnceLock<Arc<dyn Mover>> = OnceLock::new();

/// Registers the process's mover; only the first call wins (returns
/// whether this one did).
pub fn set_mover(m: Arc<dyn Mover>) -> bool {
    MOVER.set(m).is_ok()
}

/// The registered mover, if any.
pub fn mover() -> Option<&'static dyn Mover> {
    MOVER.get().map(|m| m.as_ref())
}

fn published_set() -> &'static RwLock<HashSet<(u16, u8)>> {
    static P: OnceLock<RwLock<HashSet<(u16, u8)>>> = OnceLock::new();
    P.get_or_init(|| RwLock::new(HashSet::new()))
}

/// Makes every zone whose static floors are stored so far visible to the
/// mover ([`published`]). Call it only while no rollout runs (the engine
/// does at each round's start; a single-threaded tool whenever it likes).
/// Nothing happens without a registered mover.
pub fn publish() {
    if MOVER.get().is_none() {
        return;
    }
    let zones = crate::geo::static_floor_zones();
    let mut p = published_set().write().unwrap();
    p.extend(zones);
    EPOCH.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

static EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times [`publish`] has run. A mover's own caches filled from
/// states it sees can tag entries with the epoch they were added in and
/// use only those from earlier epochs: the same round-boundary rule.
pub fn epoch() -> u64 {
    EPOCH.load(std::sync::atomic::Ordering::SeqCst)
}

/// Whether `zone`'s cached geometry may be used by the mover.
pub fn published(zone: (u16, u8)) -> bool {
    published_set().read().unwrap().contains(&zone)
}

/// Publishes `zone` directly (tests of synthetic geometry).
pub fn publish_zone(zone: (u16, u8)) {
    published_set().write().unwrap().insert(zone);
}
