//! Walkable ("geodesic") distance over a level's floors.
//!
//! Straight-line distance misleads the search on terrain with dead ends: a
//! ledge right under a summit is "close" to it, though the way up starts
//! somewhere else. This module builds, per (level, area), a navigation graph
//! from the area's static floor triangles, read from the game's memory
//! (`gStaticSurfacePartition`, surface_load.c), and answers walkable distance
//! to a target from a distance field (Dijkstra from the target), cached per
//! target node.
//!
//! The graph: floor samples on a [`CELL`]-unit XZ grid (one node per floor
//! layer of a column; a triangle no grid point falls in gets a node at its
//! centroid). Edges join nodes of neighbouring columns: any drop; a rise of
//! at most [`JUMP`], at a cost: a jump costs extra, and climbing from a steep
//! floor (where he slides) costs [`STEEP_FACTOR`] times the distance. Walls and moving platforms are ignored.
//!
//! The mesh is built by [`observe`] the first time a caller passes a state
//! where Mario is in control on a static floor of the area; until then, and
//! where either point is off the graph, [`St::path_dist`] is the straight
//! distance.
//!
//! Not used by the tree search (objectives, engine, GA, steering): its
//! heuristics stay straight-line by choice, since walking distance is wrong
//! in inconsistent ways (wall kicks, platforms, thin walls, low ceilings)
//! and would bias the search toward walking. Only the route bench's `geo`
//! method and the `geodiag` tool call [`observe`]; nothing builds a mesh
//! otherwise.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::{St, dist};

/// Grid spacing of floor samples.
pub const CELL: f32 = 100.0;
/// Highest rise one edge may climb (about a double jump).
pub const JUMP: f32 = 300.0;
/// Extra cost of a rise steeper than a walkable slope (a jump).
const JUMP_COST: f32 = 150.0;
/// Cost multiplier of climbing on a steep floor.
const STEEP_FACTOR: f32 = 3.0;
/// BBH's rooms 1 to 13 (the lobby, wings, library, basement and outside);
/// 14 is the stairs joining rooms 1 and 2 and 15 to 32 are doorways.
const REAL_ROOMS: i8 = 13;
/// Added to the straight distance where the target can't be walked to.
const UNREACHABLE: f32 = 20000.0;
/// Added to the straight distance where Mario is over no floor (a pit,
/// a moving platform far from static floors).
const OFF_GRAPH: f32 = 5000.0;
/// How far (in columns) a point off the graph looks for a floor below it.
const LOCATE_RINGS: i32 = 8;
/// Columns span the level boundary (LEVEL_BOUNDARY_MAX 0x2000) and a margin.
const HALF: i32 = 84;
const SIDE: i32 = 2 * HALF + 1;
/// Bytes of fields kept per mesh before its cache is cleared.
// 64 MB cleared every few hundred calls once v4m's objectives asked for walking distances to many targets (t1 step
// 696, SL: 37 k frames/s, planning 60% of the time, 5 of 16 cores busy; step 695: 222 k).
const FIELD_BYTES: usize = 1 << 30;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turns geodesic distances on (`OBRRT_GEO`); off, [`St::path_dist`] is the
/// straight distance and nothing is built.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

static STEER: AtomicBool = AtomicBool::new(false);

/// Whether ground steering heads for the next waypoint along the field
/// (`OBRRT_GEO_STEER`).
pub fn set_steer(on: bool) {
    STEER.store(on, Ordering::Relaxed);
}

pub fn steer_enabled() -> bool {
    enabled() && STEER.load(Ordering::Relaxed)
}

#[derive(Clone, Copy)]
struct Node {
    pos: [f32; 3],
    steep: bool,
    room: i8,
}

/// One area's navigation graph.
pub struct Mesh {
    nodes: Vec<Node>,
    /// Per column (`col_index`): the range of its nodes in `nodes`, sorted by y.
    cols: Vec<(u32, u16)>,
    /// Forward edges a -> b (CSR) with costs.
    fwd_start: Vec<u32>,
    fwd: Vec<(u32, f32)>,
    /// Reverse edges: for b, every a with a -> b.
    rev_start: Vec<u32>,
    rev: Vec<(u32, f32)>,
    fields: Mutex<HashMap<u32, Arc<[f32]>>>,
    pub triangles: usize,
}

fn col_of(x: f32, z: f32) -> Option<(i32, i32)> {
    let (ix, iz) = ((x / CELL).round() as i32, (z / CELL).round() as i32);
    (ix.abs() <= HALF && iz.abs() <= HALF).then_some((ix, iz))
}

fn col_index(ix: i32, iz: i32) -> usize {
    ((ix + HALF) * SIDE + (iz + HALF)) as usize
}

/// A static floor triangle: vertices, normal, type.
pub struct Tri {
    pub v: [[f32; 3]; 3],
    pub n: [f32; 3],
    pub kind: i16,
    /// `Surface::room` (surface_load.c; 0 where the level has no rooms or the
    /// surface is shared). BBH uses it to keep the walk out of walls
    /// ([`Mesh::build_rooms`]).
    pub room: i8,
}

/// Floors Mario can't stand on or that kill him.
fn excluded(kind: i16) -> bool {
    matches!(kind, 0x01 | 0x0A | 0x12 | 0x22 | 0x23 | 0x24 | 0x2D | 0x38)
}

/// The slope past which Mario slides instead of walking up
/// (mario_floor_is_steep, mario.c), by the floor's slipperiness.
fn steep_limit(kind: i16) -> f32 {
    match kind {
        0x13 | 0x36 | 0x73 | 0x74 | 0x75 | 0x78 => 0.9659258,
        0x14 | 0x2A | 0x35 | 0x79 | 0x2E => 0.8660254,
        0x15 | 0x37 => 0.0, // not slippery: never slides
        _ => 0.7880108,
    }
}

impl Mesh {
    pub fn build(tris: &[Tri]) -> Mesh {
        Mesh::build_rooms(tris, false)
    }

    /// [`Mesh::build`]; with `rooms`, no edge joins two different rooms
    /// numbered 1 to [`REAL_ROOMS`] (BBH: walls and closed doors between
    /// rooms, which the floor graph alone walks through; the doorways are
    /// floors of rooms 14 and up, which stay joined to their neighbours,
    /// bbh/areas/1/room.inc.c).
    pub fn build_rooms(tris: &[Tri], rooms: bool) -> Mesh {
        let mut raw: Vec<(usize, Node)> = Vec::new();
        for t in tris {
            if excluded(t.kind) || t.n[1] < 0.01 {
                continue;
            }
            let steep = t.n[1] < steep_limit(t.kind);
            let off = -(t.n[0] * t.v[0][0] + t.n[1] * t.v[0][1] + t.n[2] * t.v[0][2]);
            let y_at = |x: f32, z: f32| -(t.n[0] * x + t.n[2] * z + off) / t.n[1];
            let (mut x0, mut x1, mut z0, mut z1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
            for v in &t.v {
                x0 = x0.min(v[0]);
                x1 = x1.max(v[0]);
                z0 = z0.min(v[2]);
                z1 = z1.max(v[2]);
            }
            let mut any = false;
            let (ix0, ix1) = ((x0 / CELL).ceil() as i32, (x1 / CELL).floor() as i32);
            let (iz0, iz1) = ((z0 / CELL).ceil() as i32, (z1 / CELL).floor() as i32);
            for ix in ix0.max(-HALF)..=ix1.min(HALF) {
                for iz in iz0.max(-HALF)..=iz1.min(HALF) {
                    let (x, z) = (ix as f32 * CELL, iz as f32 * CELL);
                    if inside(t, x, z) {
                        any = true;
                        raw.push((col_index(ix, iz), Node { pos: [x, y_at(x, z), z], steep, room: t.room }));
                    }
                }
            }
            if !any {
                let c = [0, 1, 2].map(|k| (t.v[0][k] + t.v[1][k] + t.v[2][k]) / 3.0);
                if let Some((ix, iz)) = col_of(c[0], c[2]) {
                    raw.push((col_index(ix, iz), Node { pos: c, steep, room: t.room }));
                }
            }
        }
        raw.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.pos[1].total_cmp(&b.1.pos[1])));
        // Merge samples of one column within 20 units of height (shared
        // edges of neighbouring triangles); a merged node is steep only if
        // all its samples are.
        let mut nodes: Vec<Node> = Vec::with_capacity(raw.len());
        let mut cols = vec![(0u32, 0u16); (SIDE * SIDE) as usize];
        let mut i = 0;
        while i < raw.len() {
            let c = raw[i].0;
            let start = nodes.len();
            while i < raw.len() && raw[i].0 == c {
                let n = raw[i].1;
                let in_col = nodes.len() > start;
                match nodes.last_mut() {
                    Some(last) if in_col && n.pos[1] - last.pos[1] < 20.0 => {
                        last.steep &= n.steep;
                        // A column sample shared with a doorway stays a doorway.
                        if !(1..=REAL_ROOMS).contains(&n.room) {
                            last.room = n.room;
                        }
                        last.pos[1] = last.pos[1].max(n.pos[1]);
                    }
                    _ => nodes.push(n),
                }
                i += 1;
            }
            cols[c] = (start as u32, (nodes.len() - start).min(u16::MAX as usize) as u16);
        }
        // Edges between neighbouring columns.
        let mut fwd_lists: Vec<Vec<(u32, f32)>> = vec![Vec::new(); nodes.len()];
        let max_h = CELL * 2.2;
        for ix in -HALF..=HALF {
            for iz in -HALF..=HALF {
                let (s, n) = cols[col_index(ix, iz)];
                for a in s..s + n as u32 {
                    let na = nodes[a as usize];
                    for (dx, dz) in [(-1, -1), (-1, 0), (-1, 1), (0, -1), (0, 1), (1, -1), (1, 0), (1, 1)] {
                        let (jx, jz) = (ix + dx, iz + dz);
                        if jx.abs() > HALF || jz.abs() > HALF {
                            continue;
                        }
                        let (s2, n2) = cols[col_index(jx, jz)];
                        for b in s2..s2 + n2 as u32 {
                            let nb = nodes[b as usize];
                            if rooms && na.room != nb.room && (1..=REAL_ROOMS).contains(&na.room) && (1..=REAL_ROOMS).contains(&nb.room) {
                                continue;
                            }
                            let h = ((na.pos[0] - nb.pos[0]).powi(2) + (na.pos[2] - nb.pos[2]).powi(2)).sqrt();
                            if h > max_h {
                                continue;
                            }
                            let dy = nb.pos[1] - na.pos[1];
                            if dy > JUMP {
                                continue;
                            }
                            // A rise steeper than ~40 degrees takes a jump;
                            // any rise from a steep floor is a fight against
                            // sliding (steep jumps), slow and unsure.
                            let d = (h * h + dy * dy).sqrt();
                            let cost = if dy <= 5.0 {
                                d
                            } else if na.steep {
                                STEEP_FACTOR * d + JUMP_COST
                            } else if dy > 0.85 * h {
                                d + JUMP_COST
                            } else {
                                d
                            };
                            fwd_lists[a as usize].push((b, cost));
                        }
                    }
                }
            }
        }
        let mut rev_lists: Vec<Vec<(u32, f32)>> = vec![Vec::new(); nodes.len()];
        for (a, l) in fwd_lists.iter().enumerate() {
            for &(b, c) in l {
                rev_lists[b as usize].push((a as u32, c));
            }
        }
        let csr = |lists: Vec<Vec<(u32, f32)>>| {
            let mut start = Vec::with_capacity(lists.len() + 1);
            let mut flat = Vec::new();
            for l in lists {
                start.push(flat.len() as u32);
                flat.extend(l);
            }
            start.push(flat.len() as u32);
            (start, flat)
        };
        let (fwd_start, fwd) = csr(fwd_lists);
        let (rev_start, rev) = csr(rev_lists);
        Mesh { nodes, cols, fwd_start, fwd, rev_start, rev, fields: Mutex::new(HashMap::new()), triangles: tris.len() }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn edges(&self) -> usize {
        self.fwd.len()
    }

    /// The node a point stands over: the floor at most 60 above it nearest
    /// to it (counting height twice) among the 5x5 columns around it, taking
    /// in each column only the highest such floor; failing that, the same
    /// out to [`LOCATE_RINGS`] columns (over a gap).
    fn locate(&self, p: [f32; 3]) -> Option<u32> {
        self.locate_within(p, 2).or_else(|| self.locate_within(p, LOCATE_RINGS))
    }

    fn locate_within(&self, p: [f32; 3], rings: i32) -> Option<u32> {
        let (ix, iz) = col_of(p[0], p[2])?;
        // The floor just below the point, preferring near columns: score is
        // horizontal distance plus twice the height above the floor.
        let mut best = None;
        let mut bd = f32::MAX;
        let first = if rings > 2 { 3 } else { 0 };
        for r in first..=rings {
            for jx in ix - r..=ix + r {
                for jz in iz - r..=iz + r {
                    if (jx - ix).abs() != r && (jz - iz).abs() != r || jx.abs() > HALF || jz.abs() > HALF {
                        continue;
                    }
                    let (s, n) = self.cols[col_index(jx, jz)];
                    for a in (s..s + n as u32).rev() {
                        let q = self.nodes[a as usize].pos;
                        if q[1] <= p[1] + 60.0 {
                            let h = ((p[0] - q[0]).powi(2) + (p[2] - q[2]).powi(2)).sqrt();
                            let d = h + 2.0 * (p[1] - q[1]).abs();
                            if d < bd {
                                bd = d;
                                best = Some(a);
                            }
                            break;
                        }
                    }
                }
            }
            // Ring r+1 is at least r*CELL away horizontally.
            if best.is_some() && bd < r as f32 * CELL + CELL * 0.5 {
                return best;
            }
        }
        best
    }

    /// Diagnostics: (nodes that reach `t`, their lowest y; nodes reachable
    /// from `p`, their highest point).
    pub fn debug_reach(&self, p: [f32; 3], t: [f32; 3]) -> Option<(usize, f32, usize, [f32; 3])> {
        let tn = self.locate(t)?;
        let pn = self.locate(p)?;
        let f = self.compute_field(tn);
        let into: Vec<usize> = (0..f.len()).filter(|&i| f[i].is_finite()).collect();
        let low = into.iter().map(|&i| self.nodes[i].pos[1]).fold(f32::MAX, f32::min);
        let mut seen = vec![false; self.nodes.len()];
        let mut stack = vec![pn];
        seen[pn as usize] = true;
        let mut high = self.nodes[pn as usize].pos;
        let mut n = 0;
        while let Some(a) = stack.pop() {
            n += 1;
            if self.nodes[a as usize].pos[1] > high[1] {
                high = self.nodes[a as usize].pos;
            }
            let (s, e) = (self.fwd_start[a as usize] as usize, self.fwd_start[a as usize + 1] as usize);
            for &(b, _) in &self.fwd[s..e] {
                if !seen[b as usize] {
                    seen[b as usize] = true;
                    stack.push(b);
                }
            }
        }
        if std::env::var("GEO_GAP").is_ok() {
            // The closest pairs (forward-reachable, reaching t).
            let mut pairs = Vec::new();
            for (a, _) in seen.iter().enumerate().filter(|x| *x.1) {
                for &b in &into {
                    let (pa, pb) = (self.nodes[a].pos, self.nodes[b].pos);
                    let h = ((pa[0] - pb[0]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
                    if h < 260.0 {
                        pairs.push((pb[1] - pa[1], pa, self.nodes[a].steep, pb, self.nodes[b].steep));
                    }
                }
            }
            pairs.sort_by(|x, y| x.0.total_cmp(&y.0));
            for p in pairs.iter().take(15) {
                eprintln!("gap dy {:.0}: {:?} steep {} -> {:?} steep {}", p.0, p.1, p.2, p.3, p.4);
            }
        }
        Some((into.len(), low, n, high))
    }

    /// The nodes of the column at (x, z) (diagnostics).
    pub fn column(&self, x: f32, z: f32) -> Vec<([f32; 3], bool)> {
        let Some((ix, iz)) = col_of(x, z) else { return vec![] };
        let (s, n) = self.cols[col_index(ix, iz)];
        (s..s + n as u32).map(|a| (self.nodes[a as usize].pos, self.nodes[a as usize].steep)).collect()
    }

    /// The node `p` is located at (diagnostics).
    pub fn node_at(&self, p: [f32; 3]) -> Option<([f32; 3], bool)> {
        self.locate(p).map(|a| (self.nodes[a as usize].pos, self.nodes[a as usize].steep))
    }

    /// Walking distance from every node to `t` (Dijkstra over reversed edges).
    fn compute_field(&self, t: u32) -> Arc<[f32]> {
        let mut d = vec![f32::INFINITY; self.nodes.len()];
        let mut heap = BinaryHeap::new();
        d[t as usize] = 0.0;
        heap.push(Item(0.0, t));
        while let Some(Item(c, b)) = heap.pop() {
            if c > d[b as usize] {
                continue;
            }
            let (s, e) = (self.rev_start[b as usize] as usize, self.rev_start[b as usize + 1] as usize);
            for &(a, w) in &self.rev[s..e] {
                let nc = c + w;
                if nc < d[a as usize] {
                    d[a as usize] = nc;
                    heap.push(Item(nc, a));
                }
            }
        }
        d.into()
    }

    /// Calls `f` with the field of target node `t`, computing it if no
    /// thread has. A few recent fields per thread spare the shared lock (and
    /// the shared reference counts).
    fn with_field<R>(&self, t: u32, f: impl FnOnce(&[f32]) -> R) -> R {
        thread_local! {
            static RECENT: std::cell::RefCell<Vec<(usize, u32, Arc<[f32]>)>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        let me = self as *const Mesh as usize;
        RECENT.with(|r| {
            let mut r = r.borrow_mut();
            if let Some(i) = r.iter().position(|e| e.0 == me && e.1 == t) {
                return f(&r[i].2);
            }
            let cached = self.fields.lock().unwrap().get(&t).cloned();
            let field = match cached {
                Some(x) => x,
                None => {
                    let x = self.compute_field(t);
                    let mut m = self.fields.lock().unwrap();
                    if m.len() * self.nodes.len() * 4 >= FIELD_BYTES {
                        m.clear();
                    }
                    m.insert(t, x.clone());
                    x
                }
            };
            if r.len() >= 32 {
                r.remove(0);
            }
            r.push((me, t, field));
            f(&r.last().unwrap().2)
        })
    }

    /// Walking distance from `p` to `t`, if both are over the graph:
    /// `p` to its node, along the field, then the target's node to `t`;
    /// never less than the straight distance. Unreachable targets come back
    /// as the straight distance plus [`UNREACHABLE`], and points over no
    /// floor plus [`OFF_GRAPH`]. None if the target is over no floor.
    pub fn path_dist(&self, p: [f32; 3], t: [f32; 3]) -> Option<f32> {
        let tn = self.locate(t)?;
        let straight = dist(p, t);
        let Some(pn) = self.locate(p) else { return Some(straight + OFF_GRAPH) };
        let v = self.with_field(tn, |f| f[pn as usize]);
        if !v.is_finite() {
            return Some(straight + UNREACHABLE);
        }
        let d = dist(p, self.nodes[pn as usize].pos) + v + dist(self.nodes[tn as usize].pos, t);
        Some(d.max(straight))
    }

    /// A point about `ahead` units along the walking path from `p` to `t`.
    pub fn waypoint(&self, p: [f32; 3], t: [f32; 3], ahead: f32) -> Option<[f32; 3]> {
        let tn = self.locate(t)?;
        let mut a = self.locate(p)?;
        self.with_field(tn, |f| {
            if !f[a as usize].is_finite() {
                return None;
            }
            let mut walked = 0.0;
            while walked < ahead && a != tn {
                let (s, e) = (self.fwd_start[a as usize] as usize, self.fwd_start[a as usize + 1] as usize);
                let next = self.fwd[s..e].iter().min_by(|x, y| (x.1 + f[x.0 as usize]).total_cmp(&(y.1 + f[y.0 as usize])));
                let Some(&(b, w)) = next else { break };
                if f[b as usize] >= f[a as usize] {
                    break;
                }
                walked += w;
                a = b;
            }
            Some(if a == tn { t } else { self.nodes[a as usize].pos })
        })
    }
}

#[derive(PartialEq)]
struct Item(f32, u32);
impl Eq for Item {}
impl PartialOrd for Item {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Item {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.0.total_cmp(&self.0).then(o.1.cmp(&self.1))
    }
}

/// Whether (x, z) is inside the triangle's XZ projection (with a little slack).
fn inside(t: &Tri, x: f32, z: f32) -> bool {
    let [a, b, c] = t.v;
    let cross = |p: [f32; 3], q: [f32; 3]| (q[0] - p[0]) * (z - p[2]) - (q[2] - p[2]) * (x - p[0]);
    let (d1, d2, d3) = (cross(a, b), cross(b, c), cross(c, a));
    let eps = 1.0;
    let neg = d1 < -eps || d2 < -eps || d3 < -eps;
    let pos = d1 > eps || d2 > eps || d3 > eps;
    !(neg && pos)
}

type Zone = (u16, u8);

fn meshes() -> &'static RwLock<HashMap<Zone, Arc<Mesh>>> {
    static M: OnceLock<RwLock<HashMap<Zone, Arc<Mesh>>>> = OnceLock::new();
    M.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Bumped whenever a mesh is installed, so threads' cached misses expire.
static GENERATION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Calls `f` with the mesh of a zone, if built. Each thread keeps the last
/// zone's mesh (or its absence) without touching shared state.
pub fn with_mesh<R>(zone: Zone, f: impl FnOnce(&Mesh) -> R) -> Option<R> {
    thread_local! {
        static LAST: std::cell::RefCell<Option<(Zone, usize, Option<Arc<Mesh>>)>> = const { std::cell::RefCell::new(None) };
    }
    LAST.with(|l| {
        let mut l = l.borrow_mut();
        let fresh = match &*l {
            Some((z, _, Some(_))) => *z == zone,
            Some((z, g, None)) => *z == zone && *g == GENERATION.load(Ordering::Acquire),
            None => false,
        };
        if !fresh {
            let g = GENERATION.load(Ordering::Acquire);
            *l = Some((zone, g, meshes().read().unwrap().get(&zone).cloned()));
        }
        l.as_ref().unwrap().2.as_deref().map(f)
    })
}

/// The mesh of a zone, if built.
pub fn mesh(zone: Zone) -> Option<Arc<Mesh>> {
    meshes().read().unwrap().get(&zone).cloned()
}

/// Installs a mesh for a zone (tests and diagnostics).
pub fn insert(zone: Zone, m: Mesh) -> Arc<Mesh> {
    eprintln!("geo: mesh for zone {zone:?}: {} nodes, {} edges", m.len(), m.edges());
    let m = Arc::new(m);
    meshes().write().unwrap().insert(zone, m.clone());
    GENERATION.fetch_add(1, Ordering::AcqRel);
    m
}

/// Walking distance from `p` to `t` in `zone`, if geodesics are on and both
/// points are over its graph.
pub fn path_dist(zone: Zone, p: [f32; 3], t: [f32; 3]) -> Option<f32> {
    if !enabled() {
        return None;
    }
    with_mesh(zone, |m| m.path_dist(p, t)).flatten()
}

/// A waypoint about `ahead` units along the walk from `p` to `t`.
pub fn waypoint(zone: Zone, p: [f32; 3], t: [f32; 3], ahead: f32) -> Option<[f32; 3]> {
    if !enabled() {
        return None;
    }
    with_mesh(zone, |m| m.waypoint(p, t, ahead)).flatten()
}

impl St {
    /// Walking distance to `p` ([`path_dist`]) where known, else the
    /// straight distance.
    pub fn path_dist(&self, p: [f32; 3]) -> f32 {
        path_dist(self.zone(), self.pos, p).unwrap_or_else(|| self.dist(p))
    }
}

// ---------------------------------------------------------------------------
// Reading the floors from memory.

const PARTITION: usize = crate::layout::ADDR_gStaticSurfacePartition as usize;
/// 16x16 cells of 3 list heads (floors, ceilings, walls) of 8 bytes.
const CELLS: usize = 16;
const MARIO_FLOOR: usize = 0x68;
const SURFACE_OBJECT: usize = 0x2C;

fn rd_u32(m: &[u8], a: usize) -> Option<u32> {
    m.get(a..a + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}
fn rd_i16(m: &[u8], a: usize) -> Option<i16> {
    m.get(a..a + 2).map(|b| i16::from_le_bytes(b.try_into().unwrap()))
}
fn rd_i8(m: &[u8], a: usize) -> Option<i8> {
    m.get(a).map(|&b| b as i8)
}
/// `struct Surface`: s16 type, s16 force, s8 flags, s8 room (types.h).
const SURFACE_ROOM: usize = 5;
fn rd_f32(m: &[u8], a: usize) -> Option<f32> {
    rd_u32(m, a).map(f32::from_bits)
}

/// Every static floor triangle in the partition, deduplicated, with their
/// addresses.
pub fn read_floors(m: &[u8]) -> (Vec<Tri>, HashSet<u32>) {
    read_list(m, crate::layout::SPATIAL_PARTITION_FLOORS as usize)
}

/// Every static wall triangle in the partition, deduplicated.
pub fn read_walls(m: &[u8]) -> Vec<Tri> {
    read_list(m, crate::layout::SPATIAL_PARTITION_WALLS as usize).0
}

fn read_list(m: &[u8], list: usize) -> (Vec<Tri>, HashSet<u32>) {
    let mut seen = HashSet::new();
    let mut tris = Vec::new();
    for cell in 0..CELLS * CELLS {
        let head = PARTITION + cell * 24 + list * crate::layout::SURFACE_NODE_SIZE;
        let mut node = rd_u32(m, head).unwrap_or(0);
        let mut guard = 0;
        while node != 0 && guard < 100_000 {
            guard += 1;
            let Some(surf) = rd_u32(m, node as usize + 4) else { break };
            if surf != 0 && seen.insert(surf) {
                let s = surf as usize;
                let v = |k: usize| -> Option<[f32; 3]> {
                    Some([rd_i16(m, s + k)? as f32, rd_i16(m, s + k + 2)? as f32, rd_i16(m, s + k + 4)? as f32])
                };
                if let (Some(kind), Some(a), Some(b), Some(c), Some(nx), Some(ny), Some(nz)) =
                    (rd_i16(m, s), v(0x0A), v(0x10), v(0x16), rd_f32(m, s + 0x1C), rd_f32(m, s + 0x20), rd_f32(m, s + 0x24))
                {
                    tris.push(Tri { v: [a, b, c], n: [nx, ny, nz], kind, room: rd_i8(m, s + SURFACE_ROOM).unwrap_or(0) });
                }
            }
            node = rd_u32(m, node as usize).unwrap_or(0);
        }
    }
    (tris, seen)
}

// ---------------------------------------------------------------------------
// Point queries on the static partition (read-only; find_floor / find_ceil,
// surface_collision.c, without the dynamic (object) partition).

/// A static surface found by [`floor_at`] or [`ceil_at`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfHit {
    /// The surface's height at the queried (x, z).
    pub y: f32,
    /// Surface type (surface_terrains.h).
    pub kind: i16,
    pub normal: [f32; 3],
    /// Address of the `struct Surface` in the game's memory.
    pub addr: u32,
}

/// The game's cell index for a point, after its s16 cast (IDO_S16), or
/// `None` outside the level boundary.
fn cell_of(x: i32, z: i32) -> Option<usize> {
    use crate::layout::{CELL_SIZE, LEVEL_BOUNDARY_MAX, NUM_CELLS};
    let (b, c) = (LEVEL_BOUNDARY_MAX as i32, CELL_SIZE as i32);
    if x <= -b || x >= b || z <= -b || z >= b {
        return None;
    }
    let (cx, cz) = (((x + b) / c) & (NUM_CELLS as i32 - 1), ((z + b) / c) & (NUM_CELLS as i32 - 1));
    Some((cz * NUM_CELLS as i32 + cx) as usize)
}

/// Walks one static list (`list` = SPATIAL_PARTITION_*) of a cell, returning
/// the first surface that `accept`s (x, z) and its height, as
/// find_floor_from_list / find_ceil_from_list do (first match, not the
/// best one: lists are sorted by height when loaded). `floor` picks the
/// lateral test's sign.
fn first_in_list(m: &[u8], cell: usize, list: u32, x: i32, y: i32, z: i32, floor: bool) -> Option<SurfHit> {
    use crate::layout::*;
    let head = PARTITION + cell * SPATIAL_PARTITION_CELL_SIZE + list as usize * SURFACE_NODE_SIZE;
    let mut node = rd_u32(m, head)?;
    let mut guard = 0;
    while node != 0 && guard < 100_000 {
        guard += 1;
        let surf = rd_u32(m, node as usize + 4)?;
        node = rd_u32(m, node as usize)?;
        let s = surf as usize;
        let vx = |k: usize| rd_i16(m, s + 0x0A + 6 * k).map(|v| v as i32);
        let vz = |k: usize| rd_i16(m, s + 0x0A + 6 * k + 4).map(|v| v as i32);
        let (x1, z1, x2, z2, x3, z3) = (vx(0)?, vz(0)?, vx(1)?, vz(1)?, vx(2)?, vz(2)?);
        // Lateral bounds, in s32 like the game (wrapping as on the target).
        let side = |za: i32, xa: i32, zb: i32, xb: i32| {
            (za.wrapping_sub(z)).wrapping_mul(xb.wrapping_sub(xa)).wrapping_sub((xa.wrapping_sub(x)).wrapping_mul(zb.wrapping_sub(za)))
        };
        let outside = |v: i32| if floor { v < 0 } else { v > 0 };
        if outside(side(z1, x1, z2, x2)) || outside(side(z2, x2, z3, x3)) || outside(side(z3, x3, z1, x1)) {
            continue;
        }
        let kind = rd_i16(m, s + SURFACE_type)?;
        if kind as u32 == SURFACE_CAMERA_BOUNDARY {
            continue;
        }
        let n = [rd_f32(m, s + SURFACE_normal)?, rd_f32(m, s + SURFACE_normal_y)?, rd_f32(m, s + SURFACE_normal + 8)?];
        let oo = rd_f32(m, s + SURFACE_originOffset)?;
        if n[1] == 0.0 {
            continue;
        }
        let height = -(x as f32 * n[0] + n[2] * z as f32 + oo) / n[1];
        let reject = if floor { y as f32 - (height + -78.0) < 0.0 } else { y as f32 - (height - -78.0) > 0.0 };
        if reject {
            continue;
        }
        return Some(SurfHit { y: height, kind, normal: n, addr: surf });
    }
    None
}

/// The game's s16 cast of a coordinate (`(s16)(s32)x`).
fn s16_of(v: f32) -> i32 {
    (v as i32) as i16 as i32
}

/// The static (level geometry) floor the game's find_floor would return at
/// (x, y, z), ignoring object floors: the first floor in the cell's list
/// whose height is at most y + 78, skipping a SURFACE_INTANGIBLE floor to the
/// one under it (height - 200) as the game does. `None` where the game
/// returns -11000.
pub fn floor_at(m: &[u8], x: f32, y: f32, z: f32) -> Option<SurfHit> {
    use crate::layout::{SPATIAL_PARTITION_FLOORS, SURFACE_INTANGIBLE};
    let (x, y, z) = (s16_of(x), s16_of(y), s16_of(z));
    let cell = cell_of(x, z)?;
    let hit = first_in_list(m, cell, SPATIAL_PARTITION_FLOORS, x, y, z, true)?;
    if hit.kind as u32 == SURFACE_INTANGIBLE {
        return first_in_list(m, cell, SPATIAL_PARTITION_FLOORS, x, (hit.y - 200.0) as i32, z, true);
    }
    Some(hit)
}

/// The static ceiling the game's find_ceil would return at (x, y, z),
/// ignoring object ceilings: the first ceiling in the cell's list whose
/// height is at least y - 78. Mario's own query uses y = his floor's height
/// + 80 (vec3f_find_ceil in perform_ground_step / perform_air_step). `None` where the game returns 20000.
pub fn ceil_at(m: &[u8], x: f32, y: f32, z: f32) -> Option<SurfHit> {
    let (x, y, z) = (s16_of(x), s16_of(y), s16_of(z));
    first_in_list(m, cell_of(x, z)?, crate::layout::SPATIAL_PARTITION_CEILS, x, y, z, false)
}

fn building() -> &'static Mutex<HashSet<Zone>> {
    static B: OnceLock<Mutex<HashSet<Zone>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Builds the mesh of `st`'s zone from memory `m` if it isn't built yet and
/// the partition is surely this area's: Mario is in control and standing on
/// a static floor that is in it. Cheap when the zone is known.
pub fn observe(m: &[u8], st: &St) {
    if !enabled() {
        return;
    }
    let zone = st.zone();
    if with_mesh(zone, |_| ()).is_some() || !st.controllable() {
        return;
    }
    let mario = crate::layout::ADDR_gMarioStates as usize;
    let Some(floor) = rd_u32(m, mario + MARIO_FLOOR).filter(|&f| f != 0) else { return };
    if rd_u32(m, floor as usize + SURFACE_OBJECT) != Some(0) {
        return;
    }
    if !building().lock().unwrap().insert(zone) {
        return;
    }
    let (tris, seen) = read_floors(m);
    if seen.contains(&floor) {
        let t0 = std::time::Instant::now();
        let mesh = Mesh::build_rooms(&tris, zone.0 as u32 == crate::layout::LEVEL_BBH);
        if std::env::var("OBRRT_GEO_LOG").is_ok() {
            eprintln!(
                "geo: zone {:?}: {} floors, {} nodes, {} edges, built in {:.1} ms",
                zone,
                tris.len(),
                mesh.len(),
                mesh.edges(),
                t0.elapsed().as_secs_f64() * 1e3
            );
        }
        insert(zone, mesh);
    }
    building().lock().unwrap().remove(&zone);
}

// ---------------------------------------------------------------------------
// Static floor snapshot for objectives' geometry probes (read-only). Off
// until an objective calls [`want_static_floors`]; then the first state of a
// zone read with objects (a base state, standing on a static floor of the
// area) stores the zone's static floor triangles. Pure cache: reading it
// changes nothing in the game or the search.

static WANT_FLOORS: AtomicBool = AtomicBool::new(false);

fn floor_cache() -> &'static RwLock<HashMap<Zone, Arc<[Tri]>>> {
    static C: OnceLock<RwLock<HashMap<Zone, Arc<[Tri]>>>> = OnceLock::new();
    C.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Asks for static floor snapshots (see [`static_floors`]).
pub fn want_static_floors() {
    WANT_FLOORS.store(true, Ordering::Relaxed);
}

/// The zone's static floor triangles, once observed.
pub fn static_floors(zone: Zone) -> Option<Arc<[Tri]>> {
    floor_cache().read().unwrap().get(&zone).cloned()
}

fn wall_cache() -> &'static RwLock<HashMap<Zone, Arc<[Tri]>>> {
    static C: OnceLock<RwLock<HashMap<Zone, Arc<[Tri]>>>> = OnceLock::new();
    C.get_or_init(|| RwLock::new(HashMap::new()))
}

/// The zone's static wall triangles, stored with its floors.
pub fn static_walls(zone: Zone) -> Option<Arc<[Tri]>> {
    wall_cache().read().unwrap().get(&zone).cloned()
}

/// The zones whose static floors are stored, sorted.
pub fn static_floor_zones() -> Vec<Zone> {
    let mut z: Vec<Zone> = floor_cache().read().unwrap().keys().copied().collect();
    z.sort();
    z
}

/// Stores `st`'s zone's static floors from memory `m` if asked for, not
/// stored yet, and Mario is in control on a static floor of the area (so
/// the partition is this area's).
pub fn observe_floors(m: &[u8], st: &St) {
    if !WANT_FLOORS.load(Ordering::Relaxed) || !st.controllable() {
        return;
    }
    let zone = st.zone();
    if floor_cache().read().unwrap().contains_key(&zone) {
        return;
    }
    let mario = crate::layout::ADDR_gMarioStates as usize;
    let Some(floor) = rd_u32(m, mario + MARIO_FLOOR).filter(|&f| f != 0) else { return };
    if rd_u32(m, floor as usize + SURFACE_OBJECT) != Some(0) {
        return;
    }
    let (tris, seen) = read_floors(m);
    if seen.contains(&floor) {
        // Walls first: whoever sees the floors stored sees the walls too.
        wall_cache().write().unwrap().entry(zone).or_insert_with(|| read_walls(m).into());
        // Ceilings and water boxes likewise (read-only, for water-warp probes).
        ceil_cache().write().unwrap().entry(zone).or_insert_with(|| read_ceils(m).into());
        water_cache().write().unwrap().entry(zone).or_insert_with(|| read_water(m).into());
        floor_cache().write().unwrap().entry(zone).or_insert_with(|| tris.into());
    }
}

/// Every static ceiling triangle in the partition, deduplicated.
pub fn read_ceils(m: &[u8]) -> Vec<Tri> {
    read_list(m, crate::layout::SPATIAL_PARTITION_CEILS as usize).0
}

/// One environment box of `gEnvironmentRegions` (load_environmental_regions,
/// surface_load.c): `id` < 50 is water (find_water_level), multiples of 10
/// from 50 poison gas; the box is `lo < x < hi`, `lo < z < hi` (exclusive)
/// with its surface at `y`, and has no bottom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvBox {
    pub id: i16,
    pub lo: [f32; 2],
    pub hi: [f32; 2],
    pub y: f32,
}

impl EnvBox {
    pub fn is_water(&self) -> bool {
        self.id < 50
    }
    /// Whether (x, z) is inside, as find_water_level tests it.
    pub fn contains(&self, x: f32, z: f32) -> bool {
        self.lo[0] < x && x < self.hi[0] && self.lo[1] < z && z < self.hi[1]
    }
}

/// The current area's environment boxes, in the game's order (the first
/// water box containing (x, z) gives find_water_level's answer).
pub fn read_water(m: &[u8]) -> Vec<EnvBox> {
    let Some(p) = rd_u32(m, crate::layout::ADDR_gEnvironmentRegions as usize).filter(|&p| p != 0) else {
        return Vec::new();
    };
    let p = p as usize;
    let n = rd_i16(m, p).unwrap_or(0).clamp(0, 64) as usize;
    (0..n)
        .filter_map(|i| {
            let q = p + 2 + i * 12;
            let v = |k: usize| rd_i16(m, q + 2 * k);
            Some(EnvBox { id: v(0)?, lo: [v(1)? as f32, v(2)? as f32], hi: [v(3)? as f32, v(4)? as f32], y: v(5)? as f32 })
        })
        .collect()
}

fn ceil_cache() -> &'static RwLock<HashMap<Zone, Arc<[Tri]>>> {
    static C: OnceLock<RwLock<HashMap<Zone, Arc<[Tri]>>>> = OnceLock::new();
    C.get_or_init(|| RwLock::new(HashMap::new()))
}

fn water_cache() -> &'static RwLock<HashMap<Zone, Arc<[EnvBox]>>> {
    static C: OnceLock<RwLock<HashMap<Zone, Arc<[EnvBox]>>>> = OnceLock::new();
    C.get_or_init(|| RwLock::new(HashMap::new()))
}

/// The zone's static ceiling triangles, stored with its floors.
pub fn static_ceils(zone: Zone) -> Option<Arc<[Tri]>> {
    ceil_cache().read().unwrap().get(&zone).cloned()
}

/// The zone's environment boxes as they were when its floors were stored
/// (WDW's water level moves later; St::water_level has the current one).
pub fn static_water(zone: Zone) -> Option<Arc<[EnvBox]>> {
    water_cache().read().unwrap().get(&zone).cloned()
}

/// The highest floor of `tris` under (x, z) at most `y_max` high (by the
/// triangle's plane), if any.
pub fn floor_below(tris: &[Tri], x: f32, z: f32, y_max: f32) -> Option<f32> {
    let mut best: Option<f32> = None;
    for t in tris {
        let [a, b, c] = t.v;
        let side = |p: [f32; 3], q: [f32; 3]| (q[0] - p[0]) * (z - p[2]) - (q[2] - p[2]) * (x - p[0]);
        let (d1, d2, d3) = (side(a, b), side(b, c), side(c, a));
        let inside = (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0);
        if !inside || t.n[1].abs() < 1e-3 {
            continue;
        }
        let y = a[1] - (t.n[0] * (x - a[0]) + t.n[2] * (z - a[2])) / t.n[1];
        if y <= y_max && best.is_none_or(|b| y > b) {
            best = Some(y);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quad (two triangles) with corners (x0, z0)-(x1, z1), heights y at
    /// z0 and y2 at z1.
    fn quad(x0: f32, x1: f32, z0: f32, z1: f32, y: f32, y2: f32) -> [Tri; 2] {
        let n = {
            let (dz, dy) = (z1 - z0, y2 - y);
            let l = (dz * dz + dy * dy).sqrt();
            [0.0, dz / l, -dy / l]
        };
        [
            Tri { v: [[x0, y, z0], [x1, y, z0], [x1, y2, z1]], n, kind: 0, room: 0 },
            Tri { v: [[x0, y, z0], [x1, y2, z1], [x0, y2, z1]], n, kind: 0, room: 0 },
        ]
    }

    /// Fake memory holding one static floor (y 100, flat) and one ceiling
    /// (y 400, facing down) over the triangle (0,0)-(1000,0)-(0,1000)
    /// (cell (8, 8)), in the layout of surface_load.c.
    fn fake_partition() -> Vec<u8> {
        use crate::layout::*;
        let mut m = vec![0u8; PARTITION + 16 * 16 * SPATIAL_PARTITION_CELL_SIZE + 0x1000];
        let base = PARTITION + 16 * 16 * SPATIAL_PARTITION_CELL_SIZE + 0x100;
        let put32 = |m: &mut Vec<u8>, a: usize, v: u32| m[a..a + 4].copy_from_slice(&v.to_le_bytes());
        let put16 = |m: &mut Vec<u8>, a: usize, v: i16| m[a..a + 2].copy_from_slice(&v.to_le_bytes());
        let surf = |m: &mut Vec<u8>, at: usize, kind: i16, y: i16, verts: [[i16; 2]; 3], ny: f32| {
            put16(m, at + SURFACE_type, kind);
            for (k, v) in verts.iter().enumerate() {
                put16(m, at + 0x0A + 6 * k, v[0]);
                put16(m, at + 0x0A + 6 * k + 2, y);
                put16(m, at + 0x0A + 6 * k + 4, v[1]);
            }
            put32(m, at + SURFACE_normal, 0f32.to_bits());
            put32(m, at + SURFACE_normal_y, ny.to_bits());
            put32(m, at + SURFACE_normal + 8, 0f32.to_bits());
            // originOffset = -(n . v1)
            put32(m, at + SURFACE_originOffset, (-(ny * y as f32)).to_bits());
        };
        let (floor, ceil) = (base, base + SURFACE_SIZE);
        // Floors wind counter-clockwise seen from above (the game's lateral
        // test is >= 0 for them), ceilings the other way.
        surf(&mut m, floor, 0x15, 100, [[0, 0], [0, 1000], [1000, 0]], 1.0);
        surf(&mut m, ceil, 0, 400, [[0, 0], [1000, 0], [0, 1000]], -1.0);
        let (nf, nc) = (base + 4 * SURFACE_SIZE, base + 4 * SURFACE_SIZE + 8);
        put32(&mut m, nf + 4, floor as u32);
        put32(&mut m, nc + 4, ceil as u32);
        let cell = 8 * 16 + 8;
        let head = PARTITION + cell * SPATIAL_PARTITION_CELL_SIZE;
        put32(&mut m, head + SPATIAL_PARTITION_FLOORS as usize * SURFACE_NODE_SIZE, nf as u32);
        put32(&mut m, head + SPATIAL_PARTITION_CEILS as usize * SURFACE_NODE_SIZE, nc as u32);
        m
    }

    #[test]
    fn floor_and_ceil_queries() {
        let m = fake_partition();
        let f = floor_at(&m, 200.0, 150.0, 200.0).unwrap();
        assert_eq!((f.y, f.kind, f.normal[1]), (100.0, 0x15, 1.0));
        // Within the 78-unit buffer below the floor, and not below it.
        assert!(floor_at(&m, 200.0, 30.0, 200.0).is_some());
        assert!(floor_at(&m, 200.0, 20.0, 200.0).is_none());
        // Outside the triangle.
        assert!(floor_at(&m, 900.0, 150.0, 900.0).is_none());
        let c = ceil_at(&m, 200.0, 180.0, 200.0).unwrap();
        assert_eq!((c.y, c.normal[1]), (400.0, -1.0));
        assert!(ceil_at(&m, 200.0, 470.0, 200.0).is_some());
        assert!(ceil_at(&m, 200.0, 480.0, 200.0).is_none());
        assert!(ceil_at(&m, 900.0, 180.0, 900.0).is_none());
        // Outside the level boundary.
        assert!(floor_at(&m, 9000.0, 150.0, 0.0).is_none());
    }

    #[test]
    fn cliff_goes_round_by_the_ramp() {
        // Low ground at y 0 (x 0..1000, z 0..3000); a cliff to high ground at
        // y 1000 (x 1000..2000, z 0..1000); a ramp from z 3000 back up to it
        // along x 1000..2000 (z 1000..3000, rising 1000 over 2000).
        let mut tris = Vec::new();
        tris.extend(quad(0.0, 1000.0, 0.0, 3000.0, 0.0, 0.0));
        tris.extend(quad(1000.0, 2000.0, 0.0, 1000.0, 1000.0, 1000.0));
        tris.extend(quad(1000.0, 2000.0, 3000.0, 1000.0, 0.0, 1000.0));
        let m = Mesh::build(&tris);
        let foot = [900.0, 0.0, 500.0];
        let top = [1500.0, 1000.0, 500.0];
        let d = m.path_dist(foot, top).unwrap();
        // Straight: ~1166. Walking: to z ~3000, up the ramp, back: > 4000.
        assert!(d > 4000.0, "{d}");
        // Down the cliff is direct.
        let back = m.path_dist(top, foot).unwrap();
        assert!(back < 2000.0, "{back}");
    }

    /// BBH's rooms: two rooms side by side (1 and 2) are not joined by the
    /// floor graph with the room cut on; a doorway floor (room 15) between
    /// them joins them.
    #[test]
    fn rooms_meet_only_through_doorways() {
        let room = |mut q: [Tri; 2], r: i8| {
            q[0].room = r;
            q[1].room = r;
            q
        };
        let (a, b) = ([100.0, 0.0, 500.0], [900.0, 0.0, 500.0]);
        let walled: Vec<Tri> = room(quad(0.0, 500.0, 0.0, 1000.0, 0.0, 0.0), 1).into_iter().chain(room(quad(500.0, 1000.0, 0.0, 1000.0, 0.0, 0.0), 2)).collect();
        let cut = Mesh::build_rooms(&walled, true);
        assert!(cut.path_dist(a, b).unwrap() >= UNREACHABLE, "{:?}", cut.path_dist(a, b));
        assert!(Mesh::build(&walled).path_dist(a, b).unwrap() < 1000.0);
        let door: Vec<Tri> = room(quad(0.0, 450.0, 0.0, 1000.0, 0.0, 0.0), 1)
            .into_iter()
            .chain(room(quad(450.0, 550.0, 400.0, 600.0, 0.0, 0.0), 15))
            .chain(room(quad(550.0, 1000.0, 0.0, 1000.0, 0.0, 0.0), 2))
            .collect();
        let d = Mesh::build_rooms(&door, true).path_dist(a, b).unwrap();
        assert!(d < 2000.0, "{d}");
    }
}
