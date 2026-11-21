//! Records: the earliest frame any tree node got near a point, per zone and
//! progress context. A read-only query for objectives during generation
//! (`Mario::PosTime` near b: "beat the tree's record to a place it has
//! reached"); the search keeps the index and installs it.
//!
//! - The search builds a [`Records`] only if its generator asks
//!   ([`crate::Generator::wants_records`]), adds every node to it as the
//!   node is added (merge order, so the index is deterministic), and
//!   [`install`]s it on the thread that plans (generation runs there).
//! - Objectives call [`earliest_near`] (None when nothing is installed, as
//!   in labs and other drivers) and [`ctx`] (a state's progress context).
//!
//! The index: per (zone, context), a grid of [`CELL`]-unit cells, each
//! holding its nodes' (frame, position) sorted by frame. A query checks
//! the exact distance over the cells the ball around the point overlaps,
//! each in frame order, stopping at the first hit or once the frames reach
//! the best so far.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use crate::St;

pub type Zone = (u16, u8);
/// A progress context: keys, stars and (with the search's
/// OBRRT_STARSET_KEY) a hash of the collected set; see [`ctx`].
pub type Ctx = (u8, i16, u64);

/// Grid cell edge (units).
pub const CELL: f32 = 256.0;

fn cell_of(v: f32) -> i32 {
    (v / CELL).floor() as i32
}

/// Earliest arrivals near points, per (zone, context).
#[derive(Default)]
pub struct Records {
    cells: HashMap<(Zone, Ctx, [i32; 3]), Vec<(u32, [f32; 3])>>,
    n: usize,
}

impl Records {
    /// Notes that a node in `zone`, context `c`, was at `pos` at `frame`.
    pub fn insert(&mut self, zone: Zone, c: Ctx, pos: [f32; 3], frame: u32) {
        if !pos.iter().all(|v| v.is_finite()) {
            return;
        }
        let v = self.cells.entry((zone, c, [cell_of(pos[0]), cell_of(pos[1]), cell_of(pos[2])])).or_default();
        let i = v.partition_point(|e| e.0 <= frame);
        v.insert(i, (frame, pos));
        self.n += 1;
    }

    /// The earliest frame of a node in `zone` and context `c` within
    /// `radius` of `p` (inclusive), if any.
    pub fn earliest_near(&self, zone: Zone, c: Ctx, p: [f32; 3], radius: f32) -> Option<u32> {
        let r2 = radius * radius;
        let lo = [cell_of(p[0] - radius), cell_of(p[1] - radius), cell_of(p[2] - radius)];
        let hi = [cell_of(p[0] + radius), cell_of(p[1] + radius), cell_of(p[2] + radius)];
        let mut best: Option<u32> = None;
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    let Some(v) = self.cells.get(&(zone, c, [x, y, z])) else { continue };
                    for &(f, q) in v {
                        if best.is_some_and(|b| f >= b) {
                            break;
                        }
                        let d2 = (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2) + (q[2] - p[2]).powi(2);
                        if d2 <= r2 {
                            best = Some(f);
                            break;
                        }
                    }
                }
            }
        }
        best
    }

    /// Nodes noted.
    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Cells in use.
    pub fn cells(&self) -> usize {
        self.cells.len()
    }

    /// Heap bytes (approximate: the table's slots plus the cells' entries).
    pub fn bytes(&self) -> usize {
        let slot = std::mem::size_of::<((Zone, Ctx, [i32; 3]), Vec<(u32, [f32; 3])>)>() + 1;
        self.cells.capacity() * slot + self.cells.values().map(|v| v.capacity() * std::mem::size_of::<(u32, [f32; 3])>()).sum::<usize>()
    }
}

thread_local! {
    static HOOK: RefCell<Option<Arc<RwLock<Records>>>> = const { RefCell::new(None) };
}

/// Installs (or with None removes) this thread's records.
pub fn install(r: Option<Arc<RwLock<Records>>>) {
    HOOK.with(|h| *h.borrow_mut() = r);
}

/// Whether this thread has records installed.
pub fn installed() -> bool {
    HOOK.with(|h| h.borrow().is_some())
}

/// [`Records::earliest_near`] on this thread's records; None if none are
/// installed.
pub fn earliest_near(zone: Zone, c: Ctx, p: [f32; 3], radius: f32) -> Option<u32> {
    HOOK.with(|h| h.borrow().as_ref().and_then(|r| r.read().unwrap().earliest_near(zone, c, p, radius)))
}

static CTX: OnceLock<fn(&St) -> Ctx> = OnceLock::new();

/// Installs the context function (the search's; first call wins).
pub fn set_ctx_fn(f: fn(&St) -> Ctx) {
    let _ = CTX.set(f);
}

/// `s`'s progress context: the search's function if installed, else
/// (keys, stars, 0).
pub fn ctx(s: &St) -> Ctx {
    CTX.get().map_or((s.keys, s.stars, 0), |f| f(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    const Z: Zone = (16, 1);
    const C: Ctx = (0, 0, 0);

    #[test]
    fn earliest_within_radius() {
        let mut r = Records::default();
        assert_eq!(r.earliest_near(Z, C, [0.0; 3], 150.0), None);
        r.insert(Z, C, [100.0, 0.0, 0.0], 50);
        r.insert(Z, C, [140.0, 0.0, 0.0], 30);
        r.insert(Z, C, [160.0, 0.0, 0.0], 10); // outside the radius
        r.insert(Z, C, [-200.0, 0.0, 0.0], 5);
        assert_eq!(r.earliest_near(Z, C, [0.0; 3], 150.0), Some(30));
        // Across a cell edge (256): the node at 300 counts for a point at 200.
        r.insert(Z, C, [300.0, 0.0, 0.0], 3);
        assert_eq!(r.earliest_near(Z, C, [200.0, 0.0, 0.0], 150.0), Some(3));
        // Negative coordinates, inclusive radius.
        assert_eq!(r.earliest_near(Z, C, [-350.0, 0.0, 0.0], 150.0), Some(5));
        assert_eq!(r.earliest_near(Z, C, [-351.0, 0.0, 0.0], 150.0), None);
        // A later node in the same place doesn't change the record.
        r.insert(Z, C, [140.0, 0.0, 0.0], 40);
        assert_eq!(r.earliest_near(Z, C, [0.0; 3], 150.0), Some(30));
        assert_eq!(r.len(), 6);
        assert!(r.bytes() > 0);
    }

    #[test]
    fn zones_and_contexts_are_separate() {
        let mut r = Records::default();
        r.insert(Z, C, [0.0; 3], 100);
        r.insert((16, 2), C, [0.0; 3], 10);
        r.insert(Z, (0, 1, 0), [0.0; 3], 20);
        r.insert(Z, (0, 0, 7), [0.0; 3], 30);
        assert_eq!(r.earliest_near(Z, C, [10.0, 0.0, 0.0], 150.0), Some(100));
        assert_eq!(r.earliest_near((16, 2), C, [10.0, 0.0, 0.0], 150.0), Some(10));
        assert_eq!(r.earliest_near(Z, (0, 1, 0), [10.0, 0.0, 0.0], 150.0), Some(20));
        assert_eq!(r.earliest_near(Z, (0, 0, 7), [10.0, 0.0, 0.0], 150.0), Some(30));
        assert_eq!(r.earliest_near((9, 1), C, [10.0, 0.0, 0.0], 150.0), None);
    }

    #[test]
    fn hook() {
        assert!(!installed());
        assert_eq!(earliest_near(Z, C, [0.0; 3], 150.0), None);
        let r = Arc::new(RwLock::new(Records::default()));
        r.write().unwrap().insert(Z, C, [0.0; 3], 7);
        install(Some(r.clone()));
        assert_eq!(earliest_near(Z, C, [0.0; 3], 150.0), Some(7));
        r.write().unwrap().insert(Z, C, [1.0, 0.0, 0.0], 2);
        assert_eq!(earliest_near(Z, C, [0.0; 3], 150.0), Some(2));
        install(None);
        assert!(!installed());
    }
}
