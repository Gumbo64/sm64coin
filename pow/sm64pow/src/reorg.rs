//! The reorganisation cap (node policy, not consensus; v0.3, the owner's
//! decision from the fairness research, `fairness-research` LATE.md): a node
//! refuses to reorganise more than [`MAX_REORG_DEFAULT`] blocks deep. With the
//! cap, the probability of a deep reversal z(0.1 %) becomes exactly N: no
//! chain, however long, can undo more than N of our blocks. The price: after a
//! partition longer than N blocks, the two sides don't merge by themselves.
//! An operator picks a side by hand: `--sm64-max-reorg 0` (unlimited) for a
//! restart, or the RPC `sm64_allow_reorg {"hash": ...}` naming a block of the
//! other chain, which lets one reorganisation onto a chain holding that block
//! (it happens when that chain's next block arrives, or at once if the block
//! is already the alternative chain's tip and gets re-offered).
//!
//! Read next: ffi.rs.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// 36 blocks = 12 h of 20-minute blocks.
pub const MAX_REORG_DEFAULT: u64 = 36;

static MAX: AtomicU64 = AtomicU64::new(MAX_REORG_DEFAULT);

fn allowed() -> &'static Mutex<HashSet<[u8; 32]>> {
    static A: OnceLock<Mutex<HashSet<[u8; 32]>>> = OnceLock::new();
    A.get_or_init(|| Mutex::new(HashSet::new()))
}

/// The operator's cap (0: unlimited).
pub fn set_max(n: u64) {
    MAX.store(n, Ordering::Relaxed);
}

pub fn max() -> u64 {
    MAX.load(Ordering::Relaxed)
}

/// The operator allows one reorganisation onto a chain holding block `id`.
pub fn allow(id: [u8; 32]) {
    let mut a = allowed().lock().unwrap_or_else(|e| e.into_inner());
    if a.len() < 64 {
        a.insert(id);
    }
}

/// May we pop `depth` blocks of our chain to switch to the alternative chain
/// whose blocks (above the fork point) are `alt`? An allowance is used up.
pub fn allowed_reorg(depth: u64, alt: &[[u8; 32]]) -> bool {
    let m = max();
    if m == 0 || depth <= m {
        return true;
    }
    let mut a = allowed().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(id) = alt.iter().find(|id| a.contains(*id)) {
        let id = *id;
        a.remove(&id);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_reorgs_need_the_operator() {
        // (one test touches the process-wide cap)
        assert!(allowed_reorg(1, &[]) && allowed_reorg(MAX_REORG_DEFAULT, &[]));
        assert!(!allowed_reorg(MAX_REORG_DEFAULT + 1, &[[1; 32], [2; 32]]), "refused");
        allow([2; 32]);
        assert!(allowed_reorg(MAX_REORG_DEFAULT + 1, &[[1; 32], [2; 32]]), "allowed once by hash");
        assert!(!allowed_reorg(MAX_REORG_DEFAULT + 1, &[[1; 32], [2; 32]]), "used up");
        set_max(0);
        assert!(allowed_reorg(10_000, &[]), "0: unlimited");
        set_max(MAX_REORG_DEFAULT);
    }
}
