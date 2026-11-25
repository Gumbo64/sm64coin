//! EXPERIMENTAL (codec research, not in consensus): a fork of `statemix` for trying model changes.
//!
//! Original doc: codec 4 family ("statemix"): a stronger state-aware input model.
//!
//! Like [`crate`] (codec 3) every decision is predicted from the
//! game state and the pad history, but with
//!
//! * many more contexts, hashed into one table of two-rate counters;
//! * the stick predicted by rotating the previous stick with the camera
//!   (the player holds a world direction, the stick is relative to the
//!   camera) and coded as a residual to that prediction;
//! * recently used buttons and sticks as short move-to-front lists;
//! * several mixers whose weight sets are chosen by different contexts and a
//!   second mixing layer, then two APM/SSE stages, in 16-bit probabilities.
//!
//! Integer arithmetic only. [`Cfg`] holds every tunable; the shipped variants
//! are frozen presets ([`Cfg::variant`]), the codec id says which one.

use crate::Features;
use crate::logistic::{squash, stretch_table};
use crate::BitIo;
// (experimental fork: see lib.rs `experimental` feature)
use crate::trig::SIN;
use crate::GamePad;

const NCTX: usize = 18;
const NODES: usize = 4400;
const MIXERS: usize = 4;
const NIN_MAX: usize = NCTX * 2 + 1;
const TABLE_BITS: u32 = 19;

/// Model switches and rates (dev ablations; the shipped codec ids fix them).
#[derive(Clone, Debug)]
pub struct Cfg {
    pub two_rate: bool,
    /// 0 no camera prediction, 1 rotate by +dcam, 2 by -dcam.
    pub rot: u8,
    pub mru: bool,
    pub apm: bool,
    /// Number of first-layer mixers in use (1..=4).
    pub mixers: usize,
    /// Bit i set: context i enabled.
    pub ctxmask: u32,
    pub lr: i32,
    pub lr2: i32,
    pub limit: u32,
    pub fast_shift: u32,
    pub apm_rate: u32,
    pub bias_init: bool,
    pub trace: bool,
    /// Stick predictor: 0 camera rotation, 1 sweep extrapolation, 2 both.
    pub pm: u8,
    /// Code y of a far stick on the circle of the stick radius.
    pub circle: bool,
    pub stk_mru: bool,
    /// Axis decisions use only a few contexts and one mixer (speed).
    pub lite: bool,
    /// Experimental contexts: bit k enables new context 11+k (11, 12, 14, 15).
    pub xc: u32,
    /// Ring anchor: 0 the camera-rotated previous stick, 1 the previous stick itself.
    pub ringa: u8,
    /// Code a stick that lands on the full-deflection ring as an angular offset along the ring's lattice points.
    pub ring: bool,
    /// Lab: hash table bits (19 shipped).
    pub tb: u32,
    /// Lab: bit k enables extra context 14+k (spatial, velocity heading, act x stick x run, act x history).
    pub x2: u32,
    /// Lab (round 2): stick binarisation after the prediction / recently-used flags. 0 = shipped (ring/axis residual),
    /// 1 = per-axis absolute sign -> magnitude bits (MSB first), 2 = polar magnitude (shell) first then angle along the
    /// shell, 3 = polar angle (1/1024 turn bins) first, then shell, then the point in the cell.
    pub smode: u8,
    /// Lab: 0 = pad-level flags (same pad?, buttons changed?), 1 = per-button toggle channels (hazard per button
    /// with its own run length) and a separate stick-changed flag.
    pub bmode: u8,
    /// Lab: use the caller's per-frame context (`Model::ext`) as context 0 (a transmitted per-run model's leaf id).
    pub xctx: bool,
    /// Lab: shells by Chebyshev radius max(|x|,|y|) (the square rings) instead of isqrt(x^2+y^2).
    pub cheb: u32,
    /// Lab: code the shell as a signed delta from the previous stick's shell.
    pub sdelta: bool,
    /// Lab: match model (lpaq-style): the pad that followed the last occurrence of the last `mm` coded pads is tried
    /// first (0 = off).
    pub mm: u32,
}

impl Default for Cfg {
    fn default() -> Cfg {
        Cfg { two_rate: true, rot: 2, mru: false, apm: true, mixers: 2, ctxmask: 0x3FFFF, lr: 14, lr2: 6, limit: 120, fast_shift: 4, apm_rate: 6, bias_init: true, trace: false, pm: 0, circle: true, stk_mru: false, lite: false, xc: 0, ring: false, ringa: 0, tb: 19, x2: 0, smode: 0, bmode: 0, xctx: false, cheb: 0, sdelta: false, mm: 0 }
    }
}

impl Cfg {
    /// Overrides from "key=val,key=val" (dev tooling).
    pub fn parse(s: &str) -> Cfg {
        let mut c = Cfg::default();
        for kv in s.split(',').filter(|x| !x.is_empty()) {
            let (k, v) = kv.split_once('=').unwrap();
            let n: i64 = v.parse().unwrap();
            match k {
                "two" => c.two_rate = n != 0,
                "rot" => c.rot = n as u8,
                "mru" => c.mru = n != 0,
                "apm" => c.apm = n != 0,
                "mixers" => c.mixers = n as usize,
                "mask" => c.ctxmask = n as u32,
                "lr" => c.lr = n as i32,
                "lr2" => c.lr2 = n as i32,
                "limit" => c.limit = n as u32,
                "fast" => c.fast_shift = n as u32,
                "apmrate" => c.apm_rate = n as u32,
                "bias" => c.bias_init = n != 0,
                "trace" => c.trace = n != 0,
                "pm" => c.pm = n as u8,
                "circle" => c.circle = n != 0,
                "smru" => c.stk_mru = n != 0,
                "lite" => c.lite = n != 0,
                "xc" => c.xc = n as u32,
                "ring" => c.ring = n != 0,
                "ringa" => c.ringa = n as u8,
                "tb" => c.tb = n as u32,
                "x2" => c.x2 = n as u32,
                "smode" => c.smode = n as u8,
                "bmode" => c.bmode = n as u8,
                "xctx" => c.xctx = n != 0,
                "cheb" => c.cheb = n as u32,
                "sdelta" => c.sdelta = n != 0,
                "mm" => c.mm = n as u32,
                _ => panic!("unknown cfg key {k}"),
            }
        }
        c
    }
}

#[derive(Clone, Copy)]
struct Slot {
    p: u16,
    q: u16,
    n: u16,
}

const SLOT0: Slot = Slot { p: 1 << 15, q: 1 << 15, n: 0 };

struct Apm {
    t: Vec<u16>,
    rate: u32,
}

impl Apm {
    fn new(ctxs: usize, rate: u32) -> Apm {
        let mut t = vec![0u16; ctxs * 33];
        for c in 0..ctxs {
            for j in 0..33 {
                t[c * 33 + j] = (squash((j as i32 - 16) * 128) * 16).clamp(16, 65535 - 16) as u16;
            }
        }
        Apm { t, rate }
    }
    /// Refined probability (16 bits) of stretch value `st` in `ctx`; returns
    /// it with the slot to update.
    fn pp(&self, st: i32, ctx: usize) -> (u32, usize) {
        let s = (st.clamp(-2047, 2047) + 2048) as u32;
        let lo = (s >> 7) as usize;
        let w = s & 127;
        let i = ctx * 33 + lo;
        let p = (self.t[i] as u32 * (128 - w) + self.t[i + 1] as u32 * w) >> 7;
        (p, if w >= 64 { i + 1 } else { i })
    }
    fn update(&mut self, i: usize, bit: bool) {
        let g = if bit { 65535i32 } else { 0 };
        let t = self.t[i] as i32;
        self.t[i] = (t + ((g - t) >> self.rate)) as u16;
    }
}

fn stick_class(x: i8, y: i8) -> usize {
    let (ax, ay) = ((x as i32).abs(), (y as i32).abs());
    if ax < 8 && ay < 8 {
        return 0;
    }
    let dir = if ay * 5 < ax * 2 {
        if x > 0 { 0 } else { 4 }
    } else if ax * 5 < ay * 2 {
        if y > 0 { 2 } else { 6 }
    } else {
        match (x > 0, y > 0) {
            (true, true) => 1,
            (false, true) => 3,
            (false, false) => 5,
            (true, false) => 7,
        }
    };
    1 + dir * 2 + (ax.max(ay) >= 64) as usize
}

/// A signed log bucket of a stick delta, 0..=16.
fn dbk(d: i32) -> u32 {
    if d == 0 {
        return 8;
    }
    let m = (32 - d.unsigned_abs().leading_zeros()).min(8);
    if d > 0 { 8 + m } else { 8 - m }
}

/// A signed log bucket of a ring offset, 0..=22.
fn rbk(d: i32) -> u32 {
    if d == 0 {
        return 11;
    }
    let m = (32 - d.unsigned_abs().leading_zeros()).min(11);
    if d > 0 { 11 + m } else { 11 - m }
}

/// The lattice points of the full-deflection ring (distance 123.5..128.5 from the origin; most sticks a
/// bot holds at full deflection), sorted by angle (exact integer order), with the inverse map.
struct Ring {
    pts: Vec<(i8, i8)>,
    inv: Vec<u16>,
}

fn half(x: i32, y: i32) -> i32 {
    (y < 0 || (y == 0 && x < 0)) as i32
}

/// a strictly before b in counter-clockwise angle order from the +x axis (ties: shorter first).
fn ang_before(a: (i32, i32), b: (i32, i32)) -> bool {
    let (ha, hb) = (half(a.0, a.1), half(b.0, b.1));
    if ha != hb {
        return ha < hb;
    }
    let cr = a.0 * b.1 - a.1 * b.0;
    if cr != 0 {
        return cr > 0;
    }
    a.0 * a.0 + a.1 * a.1 < b.0 * b.0 + b.1 * b.1
}

impl Ring {
    fn new() -> Ring {
        let mut v: Vec<(i32, i32)> = vec![];
        for x in -128..=127i32 {
            for y in -128..=127i32 {
                let m = x * x + y * y;
                if (15253..=16512).contains(&m) {
                    v.push((x, y));
                }
            }
        }
        v.sort_by(|&a, &b| if ang_before(a, b) { std::cmp::Ordering::Less } else if ang_before(b, a) { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal });
        let mut inv = vec![u16::MAX; 65536];
        for (i, &(x, y)) in v.iter().enumerate() {
            inv[((x as u8 as usize) << 8) | y as u8 as usize] = i as u16;
        }
        Ring { pts: v.into_iter().map(|(x, y)| (x as i8, y as i8)).collect(), inv }
    }
    fn index(&self, x: i8, y: i8) -> Option<usize> {
        let i = self.inv[((x as u8 as usize) << 8) | y as u8 as usize];
        if i == u16::MAX { None } else { Some(i as usize) }
    }
    /// The first ring point at or after the direction (x, y).
    fn anchor(&self, x: i32, y: i32) -> usize {
        let i = self.pts.partition_point(|p| ang_before((p.0 as i32, p.1 as i32), (x, y)));
        i % self.pts.len()
    }
}

fn run_bucket(n: u32) -> usize {
    match n {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4..=5 => 4,
        6..=8 => 5,
        9..=15 => 6,
        _ => 7,
    }
}

fn btn_class(b: u16) -> usize {
    match b {
        0 => 0,
        0x8000 => 1,
        0x4000 => 2,
        0x2000 => 3,
        x if x & 0x8000 != 0 => 4,
        _ => 5,
    }
}

fn prev_stick(p: &GamePad) -> (i8, i8) {
    (p.stick_x, p.stick_y)
}

fn isqrt(n: i32) -> i32 {
    (n.max(0) as u32).isqrt() as i32
}

/// Direction of (x, z) in 64 sectors counter-clockwise from +x (0 for no motion).
fn sector64(x: i32, z: i32) -> u32 {
    if x == 0 && z == 0 {
        return 0;
    }
    let (q, u, v) = match (x >= 0, z >= 0) {
        (true, true) => (0u32, x as i64, z as i64),
        (false, true) => (1, z as i64, -(x as i64)),
        (false, false) => (2, -(x as i64), -(z as i64)),
        (true, false) => (3, -(z as i64), x as i64),
    };
    let sub = if v <= u { (v * 8 / (u + 1)) as u32 } else { 15 - (u * 8 / (v + 1)) as u32 };
    q * 16 + sub.min(15)
}

fn h32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^ (x >> 16)
}

/// The prediction of the stick: `s` rotated by `-d` (or `+d`) in 1/1024 turns.
fn rotate(x: i8, y: i8, d: i32, sign: i32) -> (i32, i32) {
    let k = ((sign * d + 32) >> 6) & 1023;
    let s = SIN[k as usize] as i32;
    let c = SIN[((k + 256) & 1023) as usize] as i32;
    let (x, y) = (x as i32, y as i32);
    ((x * c - y * s + 2048) >> 12, (x * s + y * c + 2048) >> 12)
}

pub struct Model {
    pub cfg: Cfg,
    /// (node, 16-bit p1, bit) of every coded decision, when `cfg.trace`.
    pub probe: Vec<(u16, u32, bool)>,
    stretch: Vec<i16>,
    rates: Vec<i32>,
    slots: Vec<Slot>,
    w1: Vec<i32>,
    w2: Vec<i32>,
    apm1: Apm,
    apm2: Apm,
    // pad history
    prev: GamePad,
    cam_at_prev: u16,
    brun: u32,
    srun: u32,
    run: u32,
    since_poke: u32,
    hist: u32,
    bhist: u32,
    shist: u32,
    lastd: (i32, i32),
    ring: Ring,
    last_rd: i32,
    last_ring: (i8, i8),
    btn_mru: [u16; 4],
    stk_mru: [(i8, i8); 4],
    // per frame
    ctxh: [u32; NCTX],
    sel: [usize; MIXERS],
    pred: (i32, i32),
    pred_b: (i32, i32),
    far: bool,
    circ: u32,
    radius: i32,
    apm_ctx: (usize, usize),
    dcb: usize,
    pdiff: usize,
    /// Lab: price decisions without learning (see [`Model::cost_pad`]).
    dry: bool,
    /// Lab: external per-frame context (cfg.xctx).
    pub ext: u32,
    shells: Shells,
    brn: [u32; 16],
    used: u16,
    last_nz: (i8, i8),
    lastlen: [[u32; 2]; 16],
    mhist: Vec<GamePad>,
    mtab: Vec<u32>,
    mptr: usize,
    mlen: u32,
}

/// Lab: every lattice point by shell (isqrt of x^2+y^2) in angle order, and by (1/1024-turn bin, shell) cell.
struct Shells {
    pts: Vec<Vec<(i8, i8)>>,
    /// per point (x as u8, y as u8): (shell, index in shell, angle bin, index in cell)
    inv: Vec<(u8, u16, u16, u8)>,
    cell: std::collections::HashMap<(u16, u8), Vec<(i8, i8)>>,
}

fn abin(x: i32, y: i32) -> u16 {
    // lab only (float atan2 to build a table; a consensus version would use an integer table)
    let a = (y as f64).atan2(x as f64) / std::f64::consts::TAU;
    (((a.rem_euclid(1.0)) * 1024.0).floor() as i32 & 1023) as u16
}

impl Shells {
    /// kind 0: Euclidean shells isqrt(x^2+y^2); 1: Chebyshev max(|x|,|y|); 2: Euclidean shells with two common
    /// full-deflection classes taken out first: 200 = the ring (distance 123.5..128.5), 201 = the square's edge
    /// (max(|x|,|y|) >= 127, not on the ring).
    fn new(kind: u8) -> Shells {
        let mut pts: Vec<Vec<(i32, i32)>> = vec![vec![]; 202];
        for x in -128..=127i32 {
            for y in -128..=127i32 {
                let m = x * x + y * y;
                let k = match kind {
                    1 => x.abs().max(y.abs()),
                    2 if (15253..=16512).contains(&m) => 200,
                    2 if x.abs().max(y.abs()) >= 127 => 201,
                    _ => isqrt(m),
                };
                pts[k as usize].push((x, y));
            }
        }
        let mut inv = vec![(0u8, 0u16, 0u16, 0u8); 65536];
        let mut cell: std::collections::HashMap<(u16, u8), Vec<(i8, i8)>> = Default::default();
        let mut out = vec![];
        for (sh, v) in pts.iter_mut().enumerate() {
            v.sort_by(|&a, &b| if ang_before(a, b) { std::cmp::Ordering::Less } else if ang_before(b, a) { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal });
            for (i, &(x, y)) in v.iter().enumerate() {
                let b = abin(x, y);
                let c = cell.entry((b, sh as u8)).or_default();
                inv[((x as u8 as usize) << 8) | y as u8 as usize] = (sh as u8, i as u16, b, c.len() as u8);
                c.push((x as i8, y as i8));
            }
            out.push(v.iter().map(|&(x, y)| (x as i8, y as i8)).collect());
        }
        Shells { pts: out, inv, cell }
    }
    fn of(&self, x: i8, y: i8) -> (u8, u16, u16, u8) {
        self.inv[((x as u8 as usize) << 8) | y as u8 as usize]
    }
}

/// Lab: the pad-history part of a [`Model`] (what `code_pad` changes besides the counters/weights).
#[derive(Clone)]
struct Hist {
    prev: GamePad,
    cam_at_prev: u16,
    brun: u32,
    srun: u32,
    run: u32,
    since_poke: u32,
    hist: u32,
    bhist: u32,
    shist: u32,
    lastd: (i32, i32),
    last_rd: i32,
    last_ring: (i8, i8),
    btn_mru: [u16; 4],
    stk_mru: [(i8, i8); 4],
    circ: u32,
    radius: i32,
    brn: [u32; 16],
    used: u16,
    last_nz: (i8, i8),
    lastlen: [[u32; 2]; 16],
}

impl Model {
    pub fn new(cfg: Cfg) -> Model {
        let tb = cfg.tb;
        let sets1 = NODES * if cfg.mixers > 2 { 37 } else { 9 };
        let w1 = vec![if cfg.two_rate { 65536 * 15 / 100 } else { 65536 * 3 / 10 }; sets1 * NIN_MAX];
        let w2 = vec![65536 / MIXERS as i32; NODES * MIXERS];
        let cfg_cheb = cfg.cheb as u8;
        let cfg_mm = cfg.mm;
        Model {
            probe: Vec::new(),
            apm1: Apm::new(NODES * 8, cfg.apm_rate),
            apm2: Apm::new(1 << 12, cfg.apm_rate),
            cfg,
            stretch: stretch_table(),
            rates: (0..=255).map(|n| 65536 * 2 / (2 * n + 3)).collect(),
            slots: vec![SLOT0; 1 << tb],
            w1,
            w2,
            prev: GamePad::default(),
            cam_at_prev: 0,
            brun: 0,
            srun: 0,
            run: 0,
            since_poke: 3,
            hist: 0,
            bhist: 0,
            shist: 0,
            lastd: (0, 0),
            ring: Ring::new(),
            last_rd: 0,
            last_ring: (0, 127),
            btn_mru: [0; 4],
            stk_mru: [(0, 0); 4],
            ctxh: [0; NCTX],
            sel: [0; MIXERS],
            pred: (0, 0),
            pred_b: (0, 0),
            far: false,
            circ: 3,
            radius: 126,
            apm_ctx: (0, 0),
            dcb: 0,
            pdiff: 0,
            dry: false,
            ext: 0,
            shells: Shells::new(cfg_cheb),
            brn: [0; 16],
            used: 0,
            last_nz: (0, 127),
            lastlen: [[0; 2]; 16],
            mhist: vec![],
            mtab: if cfg_mm > 0 { vec![u32::MAX; 1 << 20] } else { vec![] },
            mptr: 0,
            mlen: 0,
        }
    }

    fn mhash(&self) -> Option<usize> {
        let k = self.cfg.mm as usize;
        if self.mhist.len() < k {
            return None;
        }
        let mut h: u64 = 0;
        for p in &self.mhist[self.mhist.len() - k..] {
            h = (h ^ ((p.button as u64) << 16 | (p.stick_x as u8 as u64) << 8 | p.stick_y as u8 as u64)).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(23);
        }
        Some((h >> 44) as usize)
    }

    /// Lab: after coding `out`, advance the match model.
    fn match_update(&mut self, out: GamePad) {
        if self.cfg.mm == 0 {
            return;
        }
        if self.mlen > 0 && self.mptr < self.mhist.len() && self.mhist[self.mptr] == out {
            self.mlen += 1;
            self.mptr += 1;
        } else {
            self.mlen = 0;
        }
        self.mhist.push(out);
        if let Some(h) = self.mhash() {
            if self.mlen == 0 {
                let c = self.mtab[h];
                if c != u32::MAX {
                    self.mptr = c as usize;
                    self.mlen = 1;
                }
            }
            self.mtab[h] = self.mhist.len() as u32;
        }
    }

    fn hist_save(&self) -> Hist {
        Hist { prev: self.prev, cam_at_prev: self.cam_at_prev, brun: self.brun, srun: self.srun, run: self.run, since_poke: self.since_poke, hist: self.hist, bhist: self.bhist, shist: self.shist, lastd: self.lastd, last_rd: self.last_rd, last_ring: self.last_ring, btn_mru: self.btn_mru, stk_mru: self.stk_mru, circ: self.circ, radius: self.radius, brn: self.brn, used: self.used, last_nz: self.last_nz, lastlen: self.lastlen }
    }
    fn hist_load(&mut self, h: Hist) {
        (self.prev, self.cam_at_prev, self.brun, self.srun, self.run, self.since_poke, self.hist, self.bhist, self.shist) = (h.prev, h.cam_at_prev, h.brun, h.srun, h.run, h.since_poke, h.hist, h.bhist, h.shist);
        (self.lastd, self.last_rd, self.last_ring, self.btn_mru, self.stk_mru, self.circ, self.radius) = (h.lastd, h.last_rd, h.last_ring, h.btn_mru, h.stk_mru, h.circ, h.radius);
        (self.brn, self.used, self.last_nz, self.lastlen) = (h.brn, h.used, h.last_nz, h.lastlen);
    }

    /// Lab (encoder-side tool): what coding `pad` in state `f` would cost now, in bits, without changing the
    /// model (no learning, history restored). Decisions within the pad are priced with the counters as they are.
    pub fn cost_pad(&mut self, f: &Features, pad: GamePad) -> f64 {
        let h = self.hist_save();
        self.dry = true;
        let mut m = crate::CostMeter::default();
        self.code_pad(&mut m, f, pad);
        self.dry = false;
        self.hist_load(h);
        m.bits
    }

    /// Lab: cost of `pad` now plus the cost of `next` in state `f2` right after it (one frame of lookahead), no learning.
    pub fn cost_pad2(&mut self, f: &Features, pad: GamePad, f2: &Features, next: GamePad) -> f64 {
        let h = self.hist_save();
        self.dry = true;
        let mut m = crate::CostMeter::default();
        self.code_pad(&mut m, f, pad);
        self.code_pad(&mut m, f2, next);
        self.dry = false;
        self.hist_load(h);
        m.bits
    }

    /// Lab: the model's probability of "same pad as before" in state `f` (16-bit), without changing it.
    pub fn prev_pad(&self) -> GamePad {
        self.prev
    }

    /// A copy for coding a new run that keeps what was learned: the mixers
    /// and APMs whole when `weights`, each counter as only `n0` observations
    /// (`n0` = 0 forgets them), and none of the pad history.
    pub fn restart(&self, weights: bool, n0: u16) -> Model {
        let mut m = Model::new(self.cfg.clone());
        if weights {
            m.w1 = self.w1.clone();
            m.w2 = self.w2.clone();
            m.apm1.t = self.apm1.t.clone();
            m.apm2.t = self.apm2.t.clone();
        }
        if n0 > 0 {
            m.slots = self.slots.clone();
            for s in m.slots.iter_mut() {
                s.n = s.n.min(n0);
            }
        }
        m
    }

    /// A frame the perturbation poked: nothing is coded, but it is noted.
    pub fn poked(&mut self) {
        self.since_poke = 0;
    }

    fn begin(&mut self, f: &Features) {
        let act = (f.action & 0x1FF) as u32;
        let group = ((f.action >> 6) & 7) as u32;
        let speed = match f.fwd.abs() >> 8 {
            0 => 0,
            1..=7 => 1,
            8..=19 => 2,
            20..=35 => 3,
            _ => 4,
        };
        let timer = match f.action_timer {
            0 => 0,
            1 => 1,
            2..=3 => 2,
            4..=7 => 3,
            8..=15 => 4,
            _ => 5,
        };
        let pk = self.since_poke.min(3);
        let prev = self.prev;
        let pcls = stick_class(prev.stick_x, prev.stick_y) as u32;
        let rb = run_bucket(self.run) as u32;
        let btn = btn_class(prev.button) as u32;
        let dyaw = (f.face_yaw.wrapping_sub(f.cam_yaw) >> 12) as u32;
        let dcam = f.cam_yaw.wrapping_sub(self.cam_at_prev) as i16 as i32;
        self.dcb = match dcam.unsigned_abs() {
            0 => 0,
            1..=127 => 1,
            128..=511 => 2,
            512..=2047 => 3,
            _ => 4,
        };
        self.pred = if self.cfg.rot == 0 { (prev.stick_x as i32, prev.stick_y as i32) } else { rotate(prev.stick_x, prev.stick_y, dcam, if self.cfg.rot == 1 { 1 } else { -1 }) };
        self.pred_b = self.pred;
        if self.cfg.pm > 0 {
            // The stick keeps sweeping the way it did: prev * (prev / before).
            let (bx, by) = (prev.stick_x as i64, prev.stick_y as i64);
            let (ax, ay) = (self.stk_mru[0].0 as i64, self.stk_mru[0].1 as i64);
            let den = ax * ax + ay * ay;
            let (mut px, mut py) = (bx, by);
            if den > 0 && (bx, by) != (0, 0) {
                let nr = bx * ax + by * ay;
                let ni = by * ax - bx * ay;
                let div = |n: i64| (2 * n + den).div_euclid(2 * den);
                px = div(bx * nr - by * ni);
                py = div(bx * ni + by * nr);
            }
            let (px, py) = (px.clamp(-128, 127) as i32, py.clamp(-128, 127) as i32);
            self.pred_b = if self.cfg.pm == 1 { (px, py) } else { rotate(px as i8, py as i8, dcam, -1) };
            self.pred_b = (self.pred_b.0.clamp(-128, 127), self.pred_b.1.clamp(-128, 127));
        }
        self.pred = (self.pred.0.clamp(-128, 127), self.pred.1.clamp(-128, 127));
        let dp = (self.pred.0 - prev.stick_x as i32).abs().max((self.pred.1 - prev.stick_y as i32).abs());
        self.pdiff = match dp {
            0 => 0,
            1..=2 => 1,
            3..=8 => 2,
            _ => 3,
        };
        let sb = run_bucket(self.srun) as u32;
        let bb = run_bucket(self.brun) as u32;
        let vy = (f.vel[1].signum() + 1) as u32;
        let cls_m = |i: usize| btn_class(self.btn_mru[i]) as u32;
        let c = [
            if self.cfg.xctx { 0x4000_0000 | self.ext } else { 0 },
            act,
            (pcls * 8 + rb) * 6 + btn,
            (((speed * 6 + timer) * 8 + group) * 4) + pk,
            dyaw * 24 + pcls,
            (rb * 4 + pk) * 8 + group,
            self.hist & 0xFFF,
            (((prev.stick_x as u8) >> 3) as u32) * 32 + ((prev.stick_y as u8) >> 3) as u32,
            (((self.dcb as u32 * 4 + self.pdiff as u32) * 24 + pcls) * 8) + sb,
            ((btn * 6 + cls_m(0)) * 6 + cls_m(1)) * 8 + bb,
            (self.bhist & 0xFF) * 256 + (self.shist & 0xFF),
            if self.cfg.xc & 16 != 0 { (rbk(self.last_rd) * 8 + sb) * 6 + btn } else if self.cfg.xc & 1 != 0 { (dbk(self.lastd.0) * 17 + dbk(self.lastd.1)) * 8 + sb } else { 0 },
            if self.cfg.xc & 2 != 0 { (((prev.stick_x as u8) >> 2) as u32) * 64 + ((prev.stick_y as u8) >> 2) as u32 } else { 0 },
            ((vy * 16 + dyaw) * 8 + group) * 6 + btn,
            ((((f.level as u32 & 31) * 8 + (f.area as u32 & 7)) * 4096 + (((f.pos[0] >> 10) & 63) as u32) * 64 + (((f.pos[2] >> 10) & 63) as u32)) * 24) + pcls,
            (((if self.cfg.x2 & 16 != 0 { sector64(f.vel[0], f.vel[2]).wrapping_add((f.cam_yaw >> 10) as u32) } else { sector64(f.vel[0], f.vel[2]).wrapping_sub((f.cam_yaw >> 10) as u32) }) & 63) * 5 + speed) * 24 + pcls,
            (act * 24 + pcls) * 8 + rb,
            (act * 256 + (self.hist & 0xFF)) * 8 + group,
        ];
        for i in 0..NCTX {
            self.ctxh[i] = if self.cfg.ctxmask >> i & 1 != 0 { h32(c[i].wrapping_mul(0x9E37_79B1).wrapping_add(i as u32 + 1)) } else { h32(i as u32 + 1000) };
        }
        self.sel = [0, rb as usize, group as usize, pk as usize * 5 + self.dcb];
        self.apm_ctx = (pk as usize, (act as usize * 7 + group as usize * 3 + rb as usize) & 0x3FF);
        let _ = self.apm_ctx;
        self.apm_ctx = (pk as usize * 2 + (self.pdiff > 0) as usize, (act as usize * 8 + rb as usize) & 0xFFF);
    }

    fn code<B: BitIo>(&mut self, io: &mut B, node: usize, bit: bool) -> bool {
        debug_assert!(node < NODES);
        let two = self.cfg.two_rate;
        let mut idx = [0usize; NCTX];
        let mut x = [0i32; NIN_MAX];
        let mut n = 0;
        let nh = (node as u32).wrapping_mul(0x85EB_CA6B);
        const ALL: [usize; NCTX] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17];
        let nbase = 14 + (self.cfg.x2 & 15).count_ones() as usize * 0;
        let _ = nbase;
        const LITE: [usize; 6] = [0, 1, 2, 4, 8, 10];
        let lite = self.cfg.lite && node >= 48;
        let mut idv = [0usize; NCTX];
        let mut ni = 0;
        if lite {
            for &i in &LITE { idv[ni] = i; ni += 1; }
        } else {
            for &i in &ALL {
                if i < 14 || self.cfg.x2 >> (i - 14) & 1 != 0 { idv[ni] = i; ni += 1; }
            }
        }
        let ids: &[usize] = &idv[..ni];
        let nctx = ids.len();
        let tb = self.cfg.tb;
        for (j, &i) in ids.iter().enumerate() {
            let ix = ((self.ctxh[i] ^ nh).wrapping_mul(0x9E37_79B1) >> (32 - tb)) as usize;
            idx[j] = ix;
            let s = self.slots[ix];
            x[n] = self.stretch[(s.p >> 4) as usize] as i32;
            n += 1;
            if two {
                x[n] = self.stretch[(s.q >> 4) as usize] as i32;
                n += 1;
            }
        }
        x[n] = 256;
        n += 1;
        // first layer
        let m = if lite { 1 } else { self.cfg.mixers };
        let use_apm = self.cfg.apm && !lite;
        let mut base = [0usize; MIXERS];
        let mut st1 = [0i32; MIXERS];
        let mut p1 = [0i32; MIXERS];
        let offs = [0, NODES, NODES * 9, NODES * 17];
        for k in 0..m {
            let set = match k {
                0 => node,
                1 => node * 8 + self.sel[1],
                2 => node * 8 + self.sel[2],
                _ => node * 20 + self.sel[3],
            };
            base[k] = (offs[k] + set) * NIN_MAX;
            let w = &self.w1[base[k]..base[k] + n];
            let dot: i64 = x[..n].iter().zip(w).map(|(&a, &b)| a as i64 * b as i64).sum();
            st1[k] = ((dot >> 16) as i32).clamp(-2047, 2047);
            p1[k] = squash(st1[k]);
        }
        // second layer
        let wb = node * MIXERS;
        let mut dot = 0i64;
        for k in 0..m {
            dot += st1[k] as i64 * self.w2[wb + k] as i64;
        }
        let st = ((dot >> 16) as i32).clamp(-2047, 2047);
        let pm = squash(st).clamp(1, 4095);
        let mut pf = (pm as u32) << 4;
        let mut ai = (0, 0);
        if use_apm {
            let (a1, i1) = self.apm1.pp(st, node * 8 + self.apm_ctx.0);
            let (a2, i2) = self.apm2.pp(st, (self.apm_ctx.1 + node * 131) & 0xFFF);
            ai = (i1, i2);
            pf = (2 * pf + a1 + a2 + 2) >> 2;
        }
        let pf = pf.clamp(24, 65536 - 24);
        let bit = io.bit16(bit, pf);
        if self.dry {
            return bit;
        }
        if self.cfg.trace {
            self.probe.push((node as u16, pf, bit));
        }
        // updates
        let t = bit as i32;
        let e2 = ((t << 12) - pm) * self.cfg.lr2;
        for k in 0..m {
            self.w2[wb + k] += (st1[k] * e2) >> 14;
            let e1 = ((t << 12) - p1[k]) * self.cfg.lr;
            for (w, &a) in self.w1[base[k]..base[k] + n].iter_mut().zip(&x[..n]) {
                *w += (a * e1) >> 14;
            }
        }
        if use_apm {
            self.apm1.update(ai.0, bit);
            self.apm2.update(ai.1, bit);
        }
        let target = if bit { 65535i32 } else { 0 };
        for j in 0..nctx {
            let s = &mut self.slots[idx[j]];
            let rate = self.rates[s.n as usize];
            s.p = (s.p as i32 + (((target - s.p as i32) * rate) >> 16)) as u16;
            s.q = (s.q as i32 + ((target - s.q as i32) >> self.cfg.fast_shift)) as u16;
            if (s.n as u32) < self.cfg.limit {
                s.n += 1;
            }
        }
        bit
    }

    fn code_tree<B: BitIo>(&mut self, io: &mut B, node: usize, bits: u32, v: u32) -> u32 {
        let mut n = 1u32;
        for i in (0..bits).rev() {
            let b = self.code(io, node + n as usize, (v >> i) & 1 != 0);
            n = n * 2 + b as u32;
        }
        n - (1 << bits)
    }

    /// One stick axis: exactly the prediction `pred`?, else the residual
    /// to it (near: sign and distance; far: absolute value zero?, sign,
    /// magnitude bit length and mantissa).
    fn code_axis<B: BitIo>(&mut self, io: &mut B, base: usize, pred: i32, v: i8) -> i8 {
        let d = v as i32 - pred;
        if self.code(io, base, d == 0) {
            return pred as i8;
        }
        let near = self.code(io, base + 1, d.abs() <= 16);
        if near {
            let up = self.code(io, base + 2, d > 0);
            let dist = self.code_tree(io, base + 3, 4, (d.abs() - 1).max(0) as u32 & 15) as i32 + 1;
            return (pred + if up { dist } else { -dist }).clamp(-128, 127) as i8;
        }
        self.far = true;
        let zero = self.code(io, base + 19, v == 0);
        if zero {
            return 0;
        }
        let neg = self.code(io, base + 20, v < 0);
        let m = (v as i32).unsigned_abs().min(128);
        let e = 32 - m.leading_zeros();
        let e = self.code_tree(io, base + 21, 3, e.wrapping_sub(1)) + 1;
        let mut mag = 1u32;
        for i in (0..e - 1).rev() {
            let hi = e - 2 - i;
            let b = if hi < 2 {
                self.code(io, base + 30 + e as usize * 4 + (mag as usize & 3), (m >> i) & 1 != 0)
            } else {
                self.code(io, base + 70 + e as usize, (m >> i) & 1 != 0)
            };
            mag = mag * 2 + b as u32;
        }
        (if neg { -(mag as i32) } else { mag as i32 }).clamp(-128, 127) as i8
    }

    fn touch_mru<T: Copy + PartialEq>(mru: &mut [T; 4], v: T) {
        let pos = mru.iter().position(|&x| x == v).unwrap_or(3);
        for i in (1..=pos).rev() {
            mru[i] = mru[i - 1];
        }
        mru[0] = v;
    }

    /// Pads the model finds cheapest in state `f`, best first (encoder-side tool: the encoder may play
    /// any pad that leaves the game's future unchanged, so it asks for these and keeps the first that
    /// does). Pure: does not change the model.
    pub fn candidates(&mut self, f: &Features) -> Vec<GamePad> {
        self.begin(f);
        let prev = self.prev;
        let mut v: Vec<GamePad> = vec![prev];
        let mut push = |p: GamePad, v: &mut Vec<GamePad>| {
            if !v.contains(&p) {
                v.push(p);
            }
        };
        let preds = [(self.pred.0 as i8, self.pred.1 as i8), (self.pred_b.0 as i8, self.pred_b.1 as i8)];
        for &(x, y) in &preds {
            push(GamePad::new(prev.button, x, y), &mut v);
        }
        for &(x, y) in &self.stk_mru {
            push(GamePad::new(prev.button, x, y), &mut v);
        }
        for &b in &self.btn_mru {
            push(GamePad::new(b, prev.stick_x, prev.stick_y), &mut v);
        }
        for &b in &self.btn_mru {
            push(GamePad::new(b, preds[0].0, preds[0].1), &mut v);
        }
        v
    }

    /// Encoder-side: ring points between the anchor and where `want`'s stick lies (nearest the anchor first, 0 and
    /// then powers of two along the same direction): sticks the game may not tell apart from `want`'s, and cheaper.
    pub fn ring_candidates(&mut self, f: &Features, want: GamePad) -> Vec<GamePad> {
        self.begin(f);
        let mut v = vec![];
        let Some(ci) = self.ring.index(want.stick_x, want.stick_y) else { return v };
        let n = self.ring.pts.len() as i32;
        let base = if self.cfg.ringa == 1 { (self.prev.stick_x as i32, self.prev.stick_y as i32) } else { self.pred };
        let av = if base != (0, 0) { base } else { (self.last_ring.0 as i32, self.last_ring.1 as i32) };
        let anchor = self.ring.anchor(av.0, av.1) as i32;
        let d = (ci as i32 - anchor + n / 2).rem_euclid(n) - n / 2;
        let sg = d.signum();
        let mut k = 0;
        while k < d.abs() {
            let p = self.ring.pts[(anchor + sg * k).rem_euclid(n) as usize];
            v.push(GamePad::new(want.button, p.0, p.1));
            k = if k == 0 { 1 } else { k * 2 };
        }
        v
    }


    /// The new stick (it differs from the previous one): prediction / recently-used flags, then the binarisation.
    fn code_stick<B: BitIo>(&mut self, io: &mut B, pad: GamePad) -> (i8, i8) {
        let prev = self.prev;
        let (mut sx, mut sy) = (prev.stick_x, prev.stick_y);

                let pred = (self.pred.0 as i8, self.pred.1 as i8);
                let mut done = false;
                if (pred.0, pred.1) != (prev.stick_x, prev.stick_y) && self.code(io, 41, (pad.stick_x, pad.stick_y) == pred) {
                    sx = pred.0;
                    sy = pred.1;
                    done = true;
                }
                let pb = (self.pred_b.0 as i8, self.pred_b.1 as i8);
                if !done && self.cfg.pm > 0 && pb != (prev.stick_x, prev.stick_y) && pb != pred && self.code(io, 45, (pad.stick_x, pad.stick_y) == pb) {
                    sx = pb.0;
                    sy = pb.1;
                    done = true;
                }
                if !done && self.cfg.stk_mru {
                    let cands: Vec<(i8, i8)> = self.stk_mru.iter().copied().filter(|&s| s != (prev.stick_x, prev.stick_y) && s != pred).take(3).collect();
                    for (i, &c) in cands.iter().enumerate() {
                        if self.code(io, 42 + i, (pad.stick_x, pad.stick_y) == c) {
                            sx = c.0;
                            sy = c.1;
                            done = true;
                            break;
                        }
                    }
                }
                if !done && self.cfg.smode > 0 && self.cfg.smode != 6 {
                    (sx, sy) = self.code_stick_alt(io, pad);
                    done = true;
                }
                if !done && self.cfg.ring {
                    let cur = self.ring.index(pad.stick_x, pad.stick_y);
                    let on = self.code(io, 300, cur.is_some());
                    if on {
                        let n = self.ring.pts.len() as i32;
                        let base = if self.cfg.ringa == 1 { (prev.stick_x as i32, prev.stick_y as i32) } else { self.pred };
                        let av = if base != (0, 0) { base } else { (self.last_ring.0 as i32, self.last_ring.1 as i32) };
                        let anchor = self.ring.anchor(av.0, av.1) as i32;
                        let mut d = cur.map(|c| c as i32).unwrap_or(0) - anchor;
                        d = (d + n / 2).rem_euclid(n) - n / 2;
                        let idx;
                        if self.code(io, 301, d == 0) {
                            idx = anchor;
                            self.last_rd = 0;
                        } else {
                            let neg = self.code(io, 302, d < 0);
                            let m = d.unsigned_abs();
                            let e = 32 - m.leading_zeros();
                            let e = self.code_tree(io, 304, 4, e.wrapping_sub(1)) + 1;
                            let mut mag = 1u32;
                            for i in (0..e - 1).rev() {
                                let hi = e - 2 - i;
                                let b = if hi < 2 { self.code(io, 320 + e as usize * 4 + (mag as usize & 3), (m >> i) & 1 != 0) } else { self.code(io, 372 + e as usize, (m >> i) & 1 != 0) };
                                mag = mag * 2 + b as u32;
                            }
                            let sd = if neg { -(mag as i32) } else { mag as i32 };
                            self.last_rd = sd;
                            idx = (anchor + sd).rem_euclid(n);
                        }
                        let p = self.ring.pts[idx as usize];
                        sx = p.0;
                        sy = p.1;
                        self.last_ring = p;
                        done = true;
                    }
                }
                if !done && self.cfg.smode == 6 {
                    (sx, sy) = self.code_stick_alt(io, pad);
                    done = true;
                }
                if !done {
                    let ap = if self.cfg.pm > 0 { self.pred_b } else { self.pred };
                    self.far = false;
                    sx = self.code_axis(io, 48, ap.0, pad.stick_x);
                    if self.cfg.circle && self.far && self.circ & 3 != 0 {
                        // A jump: the stick is on (about) the circle, so y is nearly determined by x.
                        let r = self.radius;
                        let yc = isqrt((r * r - sx as i32 * sx as i32).max(0));
                        let neg = self.code(io, 288, pad.stick_y < 0);
                        sy = self.code_axis(io, 292, if neg { -yc } else { yc }, pad.stick_y);
                    } else {
                        let ybase = if sx as i32 == ap.0 { 208 } else { 128 };
                        sy = self.code_axis(io, ybase, ap.1, pad.stick_y);
                    }
                    if self.far {
                        let mg = isqrt(sx as i32 * sx as i32 + sy as i32 * sy as i32);
                        let on = mg >= 100 && (mg - self.radius).abs() <= 3;
                        self.circ = self.circ << 1 | on as u32;
                        if mg >= 100 {
                            self.radius = mg;
                        }
                    }
                }
            
        (sx, sy)
    }

    /// Lab: an MSB-first binary tree over `nbits`, each node's context also says whether the previous value `pv`
    /// still agrees with the prefix and, if so, its next bit.
    fn code_tree_rel<B: BitIo>(&mut self, io: &mut B, base: usize, nbits: u32, v: u32, pv: Option<u32>) -> u32 {
        let mut n = 1u32;
        let mut agree = pv.is_some();
        let pv = pv.unwrap_or(0);
        for i in (0..nbits).rev() {
            let rel = if agree { 1 + ((pv >> i) & 1) as usize } else { 0 };
            let b = self.code(io, base + n as usize * 3 + rel, (v >> i) & 1 != 0);
            if agree && b as u32 != (pv >> i) & 1 {
                agree = false;
            }
            n = n * 2 + b as u32;
        }
        n - (1 << nbits)
    }

    /// Lab: a signed offset: zero?, sign, bit length (4-bit tree), mantissa (first two bits contexted).
    fn code_off<B: BitIo>(&mut self, io: &mut B, base: usize, d: i32) -> i32 {
        if self.code(io, base, d == 0) {
            return 0;
        }
        let neg = self.code(io, base + 1, d < 0);
        let m = d.unsigned_abs();
        let e = 32 - m.leading_zeros();
        let e = self.code_tree(io, base + 4, 4, e.wrapping_sub(1)) + 1;
        let mut mag = 1u32;
        for i in (0..e - 1).rev() {
            let hi = e - 2 - i;
            let b = if hi < 2 { self.code(io, base + 24 + e as usize * 4 + (mag as usize & 3), (m >> i) & 1 != 0) } else { self.code(io, base + 90 + e as usize, (m >> i) & 1 != 0) };
            mag = mag * 2 + b as u32;
        }
        if neg { -(mag as i32) } else { mag as i32 }
    }

    fn pred_dir(&self) -> (i32, i32) {
        if self.pred != (0, 0) { self.pred } else { (self.last_nz.0 as i32, self.last_nz.1 as i32) }
    }

    /// Lab binarisations of a new stick (cfg.smode 1..3).
    fn code_stick_alt<B: BitIo>(&mut self, io: &mut B, pad: GamePad) -> (i8, i8) {
        let prev = self.prev;
        match self.cfg.smode {
            1 => {
                // per axis: zero?, sign, |v| - 1 (or -v - 1) in 7 bits MSB first against the previous value
                let mut ax = |me: &mut Self, base: usize, v: i8, pv: i8| -> i8 {
                    if me.code(io, base + (pv == 0) as usize, v == 0) {
                        return 0;
                    }
                    let ps = if pv == 0 { 0 } else if pv < 0 { 1 } else { 2 };
                    let neg = me.code(io, base + 2 + ps, v < 0);
                    let m = |x: i8| if x < 0 { (-1 - x as i32) as u32 } else { (x as i32 - 1).max(0) as u32 };
                    let same_side = pv != 0 && ((pv < 0) == neg);
                    let mm = me.code_tree_rel(io, base + 10, 7, m(v), if same_side { Some(m(pv)) } else { None });
                    if neg { (-1 - mm as i32) as i8 } else { (mm as i32 + 1).min(127) as i8 }
                };
                let x = ax(self, 1500, pad.stick_x, prev.stick_x);
                let y = ax(self, 1900, pad.stick_y, prev.stick_y);
                (x, y)
            }
            2 | 3 | 4 | 6 => {
                let pz = (prev.stick_x, prev.stick_y) == (0, 0);
                if self.code(io, 2300 + pz as usize, (pad.stick_x, pad.stick_y) == (0, 0)) {
                    return (0, 0);
                }
                if self.cfg.smode == 4 {
                    // magnitude class: the full-deflection ring (shells merged, angle = ring index) or one shell
                    let pr = self.ring.index(prev.stick_x, prev.stick_y).is_some();
                    let cur = self.ring.index(pad.stick_x, pad.stick_y);
                    if self.code(io, 2304 + pr as usize, cur.is_some()) {
                        let n = self.ring.pts.len() as i32;
                        let pd = self.pred_dir();
                        let anchor = self.ring.anchor(pd.0, pd.1) as i32;
                        let d = (cur.map(|c| c as i32).unwrap_or(0) - anchor + n / 2).rem_euclid(n) - n / 2;
                        let d = self.code_off(io, 3080, d);
                        self.last_rd = d;
                        let p = self.ring.pts[(anchor + d).rem_euclid(n) as usize];
                        self.last_ring = p;
                        return p;
                    }
                }
                let (ps, _, _, _) = self.shells.of(prev.stick_x, prev.stick_y);
                let (sh, si, sb, ci) = self.shells.of(pad.stick_x, pad.stick_y);
                let pd = self.pred_dir();
                if self.cfg.smode != 3 {
                    // magnitude first: same shell?, else the shell (8 bits against the previous shell); then the angle
                    let shell = if self.code(io, 2302 + (ps >= 200) as usize * 4, sh == ps) { ps } else if self.cfg.cheb == 2 && ps != 200 && self.code(io, 2303 + (ps == 201) as usize * 4, sh == 200) { 200 } else if self.cfg.cheb == 2 && ps != 201 && sh != 200 && self.code(io, 2304 + (ps == 200) as usize * 4, sh == 201) { 201 } else if self.cfg.sdelta { (ps as i32 + self.code_off(io, 2310, sh as i32 - ps as i32)).clamp(0, 181) as u8 } else { self.code_tree_rel(io, 2310, 8, sh as u32, Some(ps as u32)) as u8 };
                    let ab = if self.cfg.smode == 4 { 3192 - 3080 } else { 0 };
                    let v = &self.shells.pts[shell as usize];
                    let n = v.len() as i32;
                    let anchor = (v.partition_point(|p| ang_before((p.0 as i32, p.1 as i32), pd)) as i32) % n.max(1);
                    let d = (si as i32 - anchor + n / 2).rem_euclid(n.max(1)) - n / 2;
                    let d = self.code_off(io, 3080 + ab + 0, d);
                    let v = &self.shells.pts[shell as usize];
                    v[(anchor + d).rem_euclid(n.max(1)) as usize]
                } else {
                    // angle first: 1/1024-turn bin against the predicted direction's, then the shell, then the point
                    let pb = abin(pd.0, pd.1) as i32;
                    let d = (sb as i32 - pb + 512).rem_euclid(1024) - 512;
                    let d = self.code_off(io, 3200, d);
                    let bin = (pb + d).rem_euclid(1024) as u16;
                    let shell = self.code_tree_rel(io, 3320, 8, sh as u32, Some(ps as u32)) as u8;
                    let cnt = self.shells.cell.get(&(bin, shell)).map(|c| c.len()).unwrap_or(1);
                    let nb = 32 - (cnt as u32 - 1).leading_zeros();
                    let idx = if nb > 0 { self.code_tree(io, 4100 + nb as usize * 16, nb, ci as u32) } else { 0 };
                    self.shells.cell.get(&(bin, shell)).and_then(|c| c.get(idx as usize).copied()).unwrap_or((0, 0))
                }
            }
            _ => unreachable!(),
        }
    }

    /// Lab (cfg.bmode 1): per-button toggle channels (hazard: context = the button's run length, finely bucketed, and
    /// whether this frame would repeat the length of its last run of the same value), then a stick-changed flag.
    fn code_chan<B: BitIo>(&mut self, io: &mut B, pad: GamePad) -> (u16, i8, i8) {
        let prev = self.prev;
        let mut button = prev.button;
        let mut any = false;
        let rb16 = |n: u32| -> usize { if n < 8 { n as usize } else { (5 + (32 - n.leading_zeros()) as usize).min(15) } };
        for b in 0..16 {
            if self.used >> b & 1 == 0 {
                continue;
            }
            let v = (prev.button >> b & 1) as usize;
            let rep = (self.brn[b] + 1 == self.lastlen[b][v]) as usize;
            let node = 1000 + b * 64 + if self.cfg.x2 & 32 != 0 { rb16(self.brn[b]) } else { run_bucket(self.brn[b]) } * 4 + v * 2 + if self.cfg.x2 & 64 != 0 { rep } else { 0 };
            if self.code(io, node, (pad.button ^ prev.button) >> b & 1 != 0) {
                button ^= 1 << b;
                any = true;
            }
        }
        let unused = !self.used;
        if self.code(io, 4330 + any as usize, pad.button & unused != 0) {
            for b in 0..16 {
                if unused >> b & 1 != 0 && self.code(io, 4340 + b, pad.button >> b & 1 != 0) {
                    button |= 1 << b;
                }
            }
            any = true;
        }
        let (mut sx, mut sy) = (prev.stick_x, prev.stick_y);
        if self.code(io, 4360 + any as usize, (pad.stick_x, pad.stick_y) != (prev.stick_x, prev.stick_y)) {
            (sx, sy) = self.code_stick(io, pad);
        }
        (button, sx, sy)
    }
    /// Codes `pad` (encoder) or decodes it (decoder; `pad` ignored) in the
    /// state `f`, and makes it the previous pad.
    pub fn code_pad<B: BitIo>(&mut self, io: &mut B, f: &Features, pad: GamePad) -> GamePad {
        self.begin(f);
        let prev = self.prev;
        let (mut button, mut sx, mut sy) = (prev.button, prev.stick_x, prev.stick_y);
        let mpred = if self.mlen > 0 && self.mptr < self.mhist.len() { Some(self.mhist[self.mptr]) } else { None };
        let mhit = match mpred {
            Some(mp) if mp != prev => {
                let lb = run_bucket(self.mlen) * 2 + (self.run > 0) as usize;
                self.code(io, 4300 + lb, pad == mp)
            }
            _ => false,
        };
        if mhit {
            let mp = mpred.unwrap();
            (button, sx, sy) = (mp.button, mp.stick_x, mp.stick_y);
        } else if self.cfg.bmode > 0 {
            (button, sx, sy) = self.code_chan(io, pad);
        } else if !self.code(io, 0, pad == prev) {
            let bchg = self.code(io, 1, pad.button != prev.button);
            if bchg {
                // recently used buttons first
                let cands: Vec<u16> = if self.cfg.mru { self.btn_mru.iter().copied().filter(|&b| b != prev.button).take(3).collect() } else { vec![] };
                let mut hit = false;
                for (i, &c) in cands.iter().enumerate() {
                    if self.code(io, 2 + i, pad.button == c) {
                        button = c;
                        hit = true;
                        break;
                    }
                }
                if !hit {
                    button = 0;
                    for i in 0..16 {
                        let pb = (prev.button >> i) & 1 != 0;
                        let b = self.code(io, 8 + i * 2 + pb as usize, (pad.button >> i) & 1 != 0);
                        button |= (b as u16) << i;
                    }
                }
            }
            let schg = if bchg { self.code(io, 40, (pad.stick_x, pad.stick_y) != (prev.stick_x, prev.stick_y)) } else { true };
            if schg {
                (sx, sy) = self.code_stick(io, pad);
            }
        }
        let out = GamePad::new(button, sx, sy);
        if out.button != prev.button {
            self.brun = 0;
            Self::touch_mru(&mut self.btn_mru, prev.button);
        } else {
            self.brun += 1;
        }
        if (out.stick_x, out.stick_y) != (prev.stick_x, prev.stick_y) {
            self.lastd = (out.stick_x as i32 - prev.stick_x as i32, out.stick_y as i32 - prev.stick_y as i32);
            self.srun = 0;
            Self::touch_mru(&mut self.stk_mru, (prev.stick_x, prev.stick_y));
        } else {
            self.srun += 1;
        }
        self.run = if out == prev { self.run + 1 } else { 0 };
        self.hist = self.hist << 1 | (out == prev) as u32;
        self.bhist = self.bhist << 1 | (out.button != prev.button) as u32;
        self.shist = self.shist << 1 | ((out.stick_x, out.stick_y) != (prev.stick_x, prev.stick_y)) as u32;
        for b in 0..16 {
            if (out.button ^ prev.button) >> b & 1 != 0 {
                self.lastlen[b][(prev.button >> b & 1) as usize] = self.brn[b] + 1;
                self.brn[b] = 0
            } else {
                self.brn[b] += 1
            }
        }
        self.used |= out.button;
        if !self.dry {
            self.match_update(out);
        }
        if (out.stick_x, out.stick_y) != (0, 0) {
            self.last_nz = (out.stick_x, out.stick_y);
        }
        self.prev = out;
        self.cam_at_prev = f.cam_yaw;
        self.since_poke = self.since_poke.saturating_add(1);
        out
    }
}

// ---------------------------------------------------------------- the codecs

use crate::{RangeDecoder, RangeEncoder, StateCodec, StateDecoder, StateEncoder};

/// EXPERIMENTAL codecs 0xE0.. (reserved experimental ids, never accepted by consensus code): this model.
pub struct Mix2Codec {
    pub id: u8,
}

/// 0xE0: statemix variant 5 (recently-used lists) as forked here; 0xE1: plus ring-coded sticks and the ring-offset context.
pub static EXPERIMENTAL: [Mix2Codec; 2] = [Mix2Codec { id: 0xE0 }, Mix2Codec { id: 0xE1 }];

pub fn experimental_codec(id: u8) -> Option<&'static dyn StateCodec> {
    EXPERIMENTAL.iter().find(|c| c.id == id).map(|c| c as &dyn StateCodec)
}

impl Mix2Codec {
    pub fn cfg(&self) -> Cfg {
        let mut c = Cfg::default();
        c.mru = true;
        c.stk_mru = true;
        if self.id == 0xE1 {
            c.ring = true;
            c.xc = 16;
        }
        c
    }
    fn model(&self) -> Model {
        Model::new(self.cfg())
    }
}

struct MixEncoder {
    m: Model,
    e: RangeEncoder,
}

impl StateEncoder for MixEncoder {
    fn pad(&mut self, f: &Features, wanted: GamePad) {
        self.m.code_pad(&mut self.e, f, wanted);
    }
    fn poked(&mut self) {
        self.m.poked();
    }
    fn finish(self: Box<Self>) -> Vec<u8> {
        self.e.finish()
    }
}

/// Decodes and re-encodes the same bits (one model serves both).
struct DecEnc<'a> {
    d: RangeDecoder<'a>,
    e: RangeEncoder,
}

impl BitIo for DecEnc<'_> {
    fn bit(&mut self, _: bool, p1: u32) -> bool {
        let b = self.d.decode(p1);
        self.e.encode(b, p1);
        b
    }
    fn bit16(&mut self, _: bool, p1: u32) -> bool {
        let b = self.d.decode16(p1);
        self.e.encode16(b, p1);
        b
    }
}

struct MixDecoder<'a> {
    m: Model,
    io: DecEnc<'a>,
}

impl StateDecoder for MixDecoder<'_> {
    fn pad(&mut self, f: &Features) -> GamePad {
        self.m.code_pad(&mut self.io, f, GamePad::default())
    }
    fn poked(&mut self) {
        self.m.poked();
    }
    fn finish(self: Box<Self>) -> Vec<u8> {
        self.io.e.finish()
    }
}

impl StateCodec for Mix2Codec {
    fn id(&self) -> u8 {
        self.id
    }
    fn name(&self) -> &'static str {
        "mix2 (experimental)"
    }
    fn encoder(&self) -> Box<dyn StateEncoder> {
        Box::new(MixEncoder { m: self.model(), e: RangeEncoder::default() })
    }
    fn decoder<'a>(&self, body: &'a [u8]) -> Box<dyn StateDecoder + 'a> {
        Box::new(MixDecoder { m: self.model(), io: DecEnc { d: RangeDecoder::new(body), e: RangeEncoder::default() } })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(n: usize, seed: u64) -> Vec<(Features, GamePad, bool)> {
        let mut s = seed;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let ring = Ring::new();
        let mut out = vec![];
        let mut pad = GamePad::default();
        for i in 0..n {
            let r = next();
            let f = Features { action: 0x0400_0440 + (r % 3) as u32, cam_yaw: (i * 37) as u16, face_yaw: (r >> 8) as u16, fwd: (r >> 20) as i32 & 0x3fff, ..Default::default() };
            match r % 7 {
                0 => pad = GamePad::new(((r >> 12) as u16) & 0xe000, pad.stick_x, pad.stick_y),
                1 => {
                    let p = ring.pts[(r >> 16) as usize % ring.pts.len()];
                    pad = GamePad::new(pad.button, p.0, p.1)
                }
                2 => pad = GamePad::new(pad.button, (r >> 24) as i8, (r >> 32) as i8),
                _ => {}
            }
            out.push((f, pad, r % 11 == 0));
        }
        out
    }

    #[test]
    fn experimental_codecs_round_trip() {
        for c in EXPERIMENTAL.iter() {
            let fr = frames(3000, 0x1234_5678_9abc_def1);
            let mut e = c.encoder();
            for (f, p, poked) in &fr {
                if *poked { e.poked() } else { e.pad(f, *p) }
            }
            let body = e.finish();
            let mut d = c.decoder(&body);
            for (f, p, poked) in &fr {
                if *poked { d.poked() } else { assert_eq!(d.pad(f), *p) }
            }
            assert_eq!(d.finish(), body, "canonical");
        }
    }

    #[test]
    fn ring_is_a_bijection_in_angle_order() {
        let r = Ring::new();
        assert!(r.pts.len() > 3000);
        for (i, p) in r.pts.iter().enumerate() {
            assert_eq!(r.index(p.0, p.1), Some(i));
            assert_eq!(r.anchor(p.0 as i32, p.1 as i32), i);
        }
    }
}
