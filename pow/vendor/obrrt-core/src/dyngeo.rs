//! Object collision (the dynamic surface partition) per state, for
//! objectives' geometry probes (`OBRRT_BLJ_SCAN_OBJECTS=1`, default off:
//! nothing is read or stored).
//!
//! `gDynamicSurfacePartition` (surface_collision.c / surface_load.c) has the
//! static partition's layout (16 x 16 cells of floor / ceiling / wall list
//! heads) and is rebuilt every frame from the loaded objects' collision
//! models; each `struct Surface` has its owning object at +0x2C.
//!
//! When a state is read with objects ([`crate::read_state`]) and the switch
//! is on, its object surfaces within [`NEAR`] of Mario (horizontally) are
//! read, tagged with the owner's pool slot, and kept with the owners' linear
//! and angular velocities ([`DynGeo`]). They are attached to the state's
//! objects `Arc` (the same allocation every clone of the state shares) in a
//! side table holding a `Weak` of it: [`of`] returns the geometry read with
//! exactly that state, so a lookup is a pure function of the state (no
//! dependence on thread timing), and an entry dies with its state's objects
//! (pruned when the table grows). Not in the tree's memory budget (it isn't
//! St's): about 24 bytes a surface.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::geo::Tri;
use crate::{Obj, St};

/// OBRRT_BLJ_SCAN_OBJECTS=1.
pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("OBRRT_BLJ_SCAN_OBJECTS").is_ok_and(|v| v == "1"))
}

/// Surfaces with a vertex within this of Mario (horizontally) are kept.
pub const NEAR: f32 = 3500.0;

/// One object surface (vertices as the game stores them, s16).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DynSurf {
    /// Owner's pool slot.
    pub slot: u16,
    /// 0 floor, 1 ceiling, 2 wall (SPATIAL_PARTITION_*).
    pub list: u8,
    pub kind: i16,
    pub v: [[i16; 3]; 3],
    pub n: [f32; 3],
}

impl DynSurf {
    pub fn tri(&self) -> Tri {
        Tri { v: self.v.map(|p| p.map(|c| c as f32)), n: self.n, kind: self.kind, room: 0 }
    }
}

/// Owner motion: (slot, velocity, angle velocity pitch/yaw/roll, face angle
/// pitch/yaw/roll).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    pub slot: u16,
    pub vel: [f32; 3],
    pub ang_vel: [i32; 3],
    pub face: [i32; 3],
}

/// A state's object collision near Mario.
#[derive(Debug, Default)]
pub struct DynGeo {
    pub surfs: Vec<DynSurf>,
    /// One per owner (sorted by slot).
    pub motion: Vec<Motion>,
}

impl DynGeo {
    /// The owners' slots (sorted).
    pub fn slots(&self) -> Vec<u16> {
        self.motion.iter().map(|m| m.slot).collect()
    }
    pub fn of_slot(&self, slot: u16) -> impl Iterator<Item = &DynSurf> {
        self.surfs.iter().filter(move |s| s.slot == slot)
    }
    pub fn motion_of(&self, slot: u16) -> Option<&Motion> {
        self.motion.iter().find(|m| m.slot == slot)
    }
}

fn rd_u32(m: &[u8], a: usize) -> Option<u32> {
    m.get(a..a + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}
fn rd_i16(m: &[u8], a: usize) -> Option<i16> {
    m.get(a..a + 2).map(|b| i16::from_le_bytes(b.try_into().unwrap()))
}
fn rd_f32(m: &[u8], a: usize) -> Option<f32> {
    rd_u32(m, a).map(f32::from_bits)
}
fn rd_i32(m: &[u8], a: usize) -> Option<i32> {
    rd_u32(m, a).map(|v| v as i32)
}

/// Reads the dynamic partition from memory `m`: surfaces within [`NEAR`]
/// of `mario` (x, z), deduplicated, in list then partition order.
pub fn read(m: &[u8], mario: [f32; 3]) -> DynGeo {
    use crate::layout::*;
    let base = ADDR_gDynamicSurfacePartition as usize;
    let pool = ADDR_gObjectPool as usize;
    let cap = OBJECT_POOL_CAPACITY as usize;
    let mut seen = std::collections::HashSet::new();
    let mut out = DynGeo::default();
    let mut slots: Vec<u16> = Vec::new();
    for list in 0..3usize {
        for cell in 0..(NUM_CELLS * NUM_CELLS) as usize {
            let head = base + cell * SPATIAL_PARTITION_CELL_SIZE + list * SURFACE_NODE_SIZE;
            let mut node = rd_u32(m, head).unwrap_or(0);
            let mut guard = 0;
            while node != 0 && guard < 20_000 {
                guard += 1;
                let Some(surf) = rd_u32(m, node as usize + 4) else { break };
                node = rd_u32(m, node as usize).unwrap_or(0);
                if surf == 0 || !seen.insert(surf) {
                    continue;
                }
                let s = surf as usize;
                let Some(o) = rd_u32(m, s + SURFACE_object).map(|o| o as usize) else { continue };
                if o < pool || o >= pool + OBJECT_SIZE * cap || (o - pool) % OBJECT_SIZE != 0 {
                    continue;
                }
                let slot = ((o - pool) / OBJECT_SIZE) as u16;
                let v = |k: usize| -> Option<[i16; 3]> { Some([rd_i16(m, s + 0x0A + 6 * k)?, rd_i16(m, s + 0x0A + 6 * k + 2)?, rd_i16(m, s + 0x0A + 6 * k + 4)?]) };
                let (Some(a), Some(b), Some(c), Some(kind)) = (v(0), v(1), v(2), rd_i16(m, s + SURFACE_type)) else { continue };
                let near = [a, b, c].iter().any(|p| (p[0] as f32 - mario[0]).hypot(p[2] as f32 - mario[2]) < NEAR);
                if !near {
                    continue;
                }
                let n = [rd_f32(m, s + SURFACE_normal).unwrap_or(0.0), rd_f32(m, s + SURFACE_normal + 4).unwrap_or(0.0), rd_f32(m, s + SURFACE_normal + 8).unwrap_or(0.0)];
                out.surfs.push(DynSurf { slot, list: list as u8, kind, v: [a, b, c], n });
                if let Err(i) = slots.binary_search(&slot) {
                    slots.insert(i, slot);
                }
            }
        }
    }
    for slot in slots {
        let o = pool + slot as usize * OBJECT_SIZE;
        let f = |a: usize| rd_f32(m, o + a).unwrap_or(0.0);
        let i = |a: usize| rd_i32(m, o + a).unwrap_or(0);
        out.motion.push(Motion {
            slot,
            vel: [f(OBJ_oVelX), f(OBJ_oVelY), f(OBJ_oVelZ)],
            // oAngleVelPitch / Yaw / Roll (0x114, 0x118, 0x11C), oFaceAngle (0xD0..).
            ang_vel: [i(OBJ_oAngleVelYaw - 4), i(OBJ_oAngleVelYaw), i(OBJ_oAngleVelYaw + 4)],
            face: [i(OBJ_oFaceAngleYaw - 4), i(OBJ_oFaceAngleYaw), i(OBJ_oFaceAngleYaw + 4)],
        });
    }
    out
}

type Table = HashMap<usize, (Weak<Vec<Obj>>, Arc<DynGeo>)>;

fn table() -> &'static Mutex<(Table, usize)> {
    static T: OnceLock<Mutex<(Table, usize)>> = OnceLock::new();
    T.get_or_init(|| Mutex::new((HashMap::new(), 4096)))
}

/// Attaches `g` to `st`'s objects (called by read_state).
pub(crate) fn attach(st: &St, g: DynGeo) {
    let Some(objs) = &st.objects else { return };
    let mut t = table().lock().unwrap();
    let (map, limit) = &mut *t;
    if map.len() >= *limit {
        map.retain(|_, (w, _)| w.strong_count() > 0);
        *limit = (map.len() * 2).max(4096);
    }
    map.insert(Arc::as_ptr(objs) as usize, (Arc::downgrade(objs), Arc::new(g)));
}

/// The object collision read with `st` (None: switch off, no objects, or
/// read without them).
pub fn of(st: &St) -> Option<Arc<DynGeo>> {
    let objs = st.objects.as_ref()?;
    let t = table().lock().unwrap();
    let (w, g) = t.0.get(&(Arc::as_ptr(objs) as usize))?;
    w.upgrade().filter(|a| Arc::ptr_eq(a, objs)).map(|_| g.clone())
}

/// Live entries (diagnostics).
pub fn entries() -> usize {
    table().lock().unwrap().0.len()
}
