//! Codecs 6 and 7 ("statemix2"): the statemix model (codec 5's contexts, mixers, APMs and
//! recently-used lists) with a different stick binarisation. FROZEN: every constant here is
//! consensus; a change is a new codec id (docs/V0.4-DESIGN.md section 1.5, COMPRESS2.md section 6).
//!
//! A stick that differs from the previous one is coded as, in order: "exactly the camera-rotated
//! previous stick?", "one of the three most recently used sticks?", then
//!
//! * "is it (0, 0)?";
//! * its shell: "same shell as the previous stick?", else the shell index as an 8-bit MSB-first tree
//!   whose nodes know whether the previous shell still agrees with the prefix;
//! * its place on the shell: every lattice point of the shell sorted in exact integer angle order
//!   (cross products), coded as a signed offset (zero?, sign, bit length, mantissa) from the first
//!   point at or after the predicted direction (the camera-rotated previous stick, or the last
//!   non-zero stick when that is (0, 0)).
//!
//! The two ids differ only in what a shell is:
//!
//! * **6 (polar)**: Euclidean, r = isqrt(x^2 + y^2). Best on miner runs, whose sticks hold full tilt
//!   with a radius of 125..127.
//! * **7 (square)**: Chebyshev, r = max(|x|, |y|) (8r points). Best on human TASes, where sticks sit
//!   on the square's edge.
//!
//! Integer arithmetic only: shells come from `isqrt` or `max`, the angle order from exact cross
//! products, the camera rotation from the integer sine table. No float anywhere, encode or decode.
//! The lab version (branch compressor-research-2, `mix2.rs`, `smode=2` / `smode=2,cheb=1`) had other
//! binarisations (ring, axis, angle-first, per-button channels, match model, target seeking) that
//! are not part of 6 and 7 and are gone here. The golden vectors in the tests below pin the output.
//!
//! A decoder never panics on a corrupt body: a shell that holds no point makes `finish` return bytes
//! that cannot equal the body, so the run is rejected as not canonical.

use crate::codec::{StateCodec, StateDecoder, StateEncoder};
use crate::logistic::{squash, stretch_table};
use crate::range::{BitIo, RangeDecoder, RangeEncoder};
use crate::trig::SIN;
use sm64coin_perturb::Pad as GamePad;
use sm64coin_perturb::sm64::Features;
use std::sync::OnceLock;

const NCTX: usize = 14;
/// Decision tree nodes; the highest one used is 3186.
const NODES: usize = 3200;
const MIXERS: usize = 4;
const NIN: usize = NCTX * 2 + 1;
const TABLE_BITS: u32 = 19;
const LR: i32 = 14;
const LR2: i32 = 6;
const LIMIT: u32 = 120;
const FAST_SHIFT: u32 = 4;
const APM_RATE: u32 = 6;

#[derive(Clone, Copy)]
struct Slot {
    p: u16,
    q: u16,
    n: u16,
}

const SLOT0: Slot = Slot { p: 1 << 15, q: 1 << 15, n: 0 };

struct Apm {
    t: Vec<u16>,
}

impl Apm {
    fn new(ctxs: usize) -> Apm {
        let mut t = vec![0u16; ctxs * 33];
        for c in 0..ctxs {
            for j in 0..33 {
                t[c * 33 + j] = (squash((j as i32 - 16) * 128) * 16).clamp(16, 65535 - 16) as u16;
            }
        }
        Apm { t }
    }
    /// Refined probability (16 bits) of stretch value `st` in `ctx`; returns it with the slot to update.
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
        self.t[i] = (t + ((g - t) >> APM_RATE)) as u16;
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

fn h32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^ (x >> 16)
}

/// The prediction of the stick: `s` rotated by `-d` in 1/1024 turns (the camera's turn).
fn rotate_back(x: i8, y: i8, d: i32) -> (i32, i32) {
    let k = ((-d + 32) >> 6) & 1023;
    let s = SIN[k as usize] as i32;
    let c = SIN[((k + 256) & 1023) as usize] as i32;
    let (x, y) = (x as i32, y as i32);
    ((x * c - y * s + 2048) >> 12, (x * s + y * c + 2048) >> 12)
}

/// Every stick lattice point by shell, each shell in angle order, with the inverse map.
struct Shells {
    pts: Vec<Vec<(i8, i8)>>,
    /// per point (x as u8, y as u8): (shell, index in shell)
    inv: Vec<(u8, u16)>,
}

impl Shells {
    fn new(square: bool) -> Shells {
        let mut pts: Vec<Vec<(i32, i32)>> = vec![vec![]; 182];
        for x in -128..=127i32 {
            for y in -128..=127i32 {
                let k = if square { x.abs().max(y.abs()) } else { ((x * x + y * y) as u32).isqrt() as i32 };
                pts[k as usize].push((x, y));
            }
        }
        let mut inv = vec![(0u8, 0u16); 65536];
        let mut out = vec![];
        for (sh, v) in pts.iter_mut().enumerate() {
            v.sort_by(|&a, &b| if ang_before(a, b) { std::cmp::Ordering::Less } else if ang_before(b, a) { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal });
            for (i, &(x, y)) in v.iter().enumerate() {
                inv[((x as u8 as usize) << 8) | y as u8 as usize] = (sh as u8, i as u16);
            }
            out.push(v.iter().map(|&(x, y)| (x as i8, y as i8)).collect());
        }
        Shells { pts: out, inv }
    }
    fn of(&self, x: i8, y: i8) -> (u8, u16) {
        self.inv[((x as u8 as usize) << 8) | y as u8 as usize]
    }
}

fn shells(square: bool) -> &'static Shells {
    static POLAR: OnceLock<Shells> = OnceLock::new();
    static SQUARE: OnceLock<Shells> = OnceLock::new();
    if square { SQUARE.get_or_init(|| Shells::new(true)) } else { POLAR.get_or_init(|| Shells::new(false)) }
}

struct Model {
    shells: &'static Shells,
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
    last_nz: (i8, i8),
    btn_mru: [u16; 4],
    stk_mru: [(i8, i8); 4],
    // per frame
    ctxh: [u32; NCTX],
    rb: usize,
    pred: (i32, i32),
    apm_ctx: (usize, usize),
    /// A decoded shell held no point (a corrupt body).
    bad: bool,
}

impl Model {
    fn new(square: bool) -> Model {
        Model {
            shells: shells(square),
            stretch: stretch_table(),
            rates: (0..=255).map(|n| 65536 * 2 / (2 * n + 3)).collect(),
            slots: vec![SLOT0; 1 << TABLE_BITS],
            w1: vec![65536 * 15 / 100; NODES * 9 * NIN],
            w2: vec![65536 / MIXERS as i32; NODES * MIXERS],
            apm1: Apm::new(NODES * 8),
            apm2: Apm::new(1 << 12),
            prev: GamePad::default(),
            cam_at_prev: 0,
            brun: 0,
            srun: 0,
            run: 0,
            since_poke: 3,
            hist: 0,
            bhist: 0,
            shist: 0,
            last_nz: (0, 127),
            btn_mru: [0; 4],
            stk_mru: [(0, 0); 4],
            ctxh: [0; NCTX],
            rb: 0,
            pred: (0, 0),
            apm_ctx: (0, 0),
            bad: false,
        }
    }

    /// A frame the perturbation poked: nothing is coded, but it is noted.
    fn poked(&mut self) {
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
        let dcb = match dcam.unsigned_abs() {
            0 => 0,
            1..=127 => 1,
            128..=511 => 2,
            512..=2047 => 3,
            _ => 4,
        };
        let pred = rotate_back(prev.stick_x, prev.stick_y, dcam);
        self.pred = (pred.0.clamp(-128, 127), pred.1.clamp(-128, 127));
        let dp = (self.pred.0 - prev.stick_x as i32).abs().max((self.pred.1 - prev.stick_y as i32).abs());
        let pdiff = match dp {
            0 => 0,
            1..=2 => 1,
            3..=8 => 2,
            _ => 3,
        };
        let sb = run_bucket(self.srun) as u32;
        let bb = run_bucket(self.brun) as u32;
        let vy = (f.vel[1].signum() + 1) as u32;
        let cls_m = |i: usize| btn_class(self.btn_mru[i]) as u32;
        let c: [u32; NCTX] = [
            0,
            act,
            (pcls * 8 + rb) * 6 + btn,
            (((speed * 6 + timer) * 8 + group) * 4) + pk,
            dyaw * 24 + pcls,
            (rb * 4 + pk) * 8 + group,
            self.hist & 0xFFF,
            (((prev.stick_x as u8) >> 3) as u32) * 32 + ((prev.stick_y as u8) >> 3) as u32,
            (((dcb as u32 * 4 + pdiff as u32) * 24 + pcls) * 8) + sb,
            ((btn * 6 + cls_m(0)) * 6 + cls_m(1)) * 8 + bb,
            (self.bhist & 0xFF) * 256 + (self.shist & 0xFF),
            0,
            0,
            ((vy * 16 + dyaw) * 8 + group) * 6 + btn,
        ];
        for i in 0..NCTX {
            self.ctxh[i] = h32(c[i].wrapping_mul(0x9E37_79B1).wrapping_add(i as u32 + 1));
        }
        self.rb = rb as usize;
        self.apm_ctx = (pk as usize * 2 + (pdiff > 0) as usize, (act as usize * 8 + rb as usize) & 0xFFF);
    }

    /// One binary decision at tree node `node`: predicted by the 14 hashed contexts, two mixers, two APMs;
    /// then everything learns the outcome.
    fn code<B: BitIo>(&mut self, io: &mut B, node: usize, bit: bool) -> bool {
        debug_assert!(node < NODES);
        let mut idx = [0usize; NCTX];
        let mut x = [0i32; NIN];
        let mut n = 0;
        let nh = (node as u32).wrapping_mul(0x85EB_CA6B);
        for i in 0..NCTX {
            let ix = ((self.ctxh[i] ^ nh).wrapping_mul(0x9E37_79B1) >> (32 - TABLE_BITS)) as usize;
            idx[i] = ix;
            let s = self.slots[ix];
            x[n] = self.stretch[(s.p >> 4) as usize] as i32;
            x[n + 1] = self.stretch[(s.q >> 4) as usize] as i32;
            n += 2;
        }
        x[n] = 256;
        n += 1;
        // first layer: two weight sets, by node and by node x run bucket
        let mut base = [0usize; 2];
        let mut st1 = [0i32; 2];
        let mut p1 = [0i32; 2];
        for k in 0..2 {
            let set = if k == 0 { node } else { NODES + node * 8 + self.rb };
            base[k] = set * NIN;
            let w = &self.w1[base[k]..base[k] + n];
            let dot: i64 = x[..n].iter().zip(w).map(|(&a, &b)| a as i64 * b as i64).sum();
            st1[k] = ((dot >> 16) as i32).clamp(-2047, 2047);
            p1[k] = squash(st1[k]);
        }
        // second layer
        let wb = node * MIXERS;
        let mut dot = 0i64;
        for k in 0..2 {
            dot += st1[k] as i64 * self.w2[wb + k] as i64;
        }
        let st = ((dot >> 16) as i32).clamp(-2047, 2047);
        let pm = squash(st).clamp(1, 4095);
        let (a1, i1) = self.apm1.pp(st, node * 8 + self.apm_ctx.0);
        let (a2, i2) = self.apm2.pp(st, (self.apm_ctx.1 + node * 131) & 0xFFF);
        let pf = ((2 * ((pm as u32) << 4) + a1 + a2 + 2) >> 2).clamp(24, 65536 - 24);
        let bit = io.bit16(bit, pf);
        // updates
        let t = bit as i32;
        let e2 = ((t << 12) - pm) * LR2;
        for k in 0..2 {
            self.w2[wb + k] += (st1[k] * e2) >> 14;
            let e1 = ((t << 12) - p1[k]) * LR;
            for (w, &a) in self.w1[base[k]..base[k] + n].iter_mut().zip(&x[..n]) {
                *w += (a * e1) >> 14;
            }
        }
        self.apm1.update(i1, bit);
        self.apm2.update(i2, bit);
        let target = if bit { 65535i32 } else { 0 };
        for &ix in &idx {
            let s = &mut self.slots[ix];
            let rate = self.rates[s.n as usize];
            s.p = (s.p as i32 + (((target - s.p as i32) * rate) >> 16)) as u16;
            s.q = (s.q as i32 + ((target - s.q as i32) >> FAST_SHIFT)) as u16;
            if (s.n as u32) < LIMIT {
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

    /// An MSB-first binary tree over `nbits`; each node's context also says whether the previous value `pv`
    /// still agrees with the prefix and, if so, its next bit.
    fn code_tree_rel<B: BitIo>(&mut self, io: &mut B, base: usize, nbits: u32, v: u32, pv: u32) -> u32 {
        let mut n = 1u32;
        let mut agree = true;
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

    /// A signed offset: zero?, sign, bit length (4-bit tree), mantissa (first two bits contexted).
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

    fn touch_mru<T: Copy + PartialEq>(mru: &mut [T; 4], v: T) {
        let pos = mru.iter().position(|&x| x == v).unwrap_or(3);
        for i in (1..=pos).rev() {
            mru[i] = mru[i - 1];
        }
        mru[0] = v;
    }

    /// The new stick (it differs from the previous one).
    fn code_stick<B: BitIo>(&mut self, io: &mut B, pad: GamePad) -> (i8, i8) {
        let prev = self.prev;
        let ps = (prev.stick_x, prev.stick_y);
        let want = (pad.stick_x, pad.stick_y);
        let pred = (self.pred.0 as i8, self.pred.1 as i8);
        if pred != ps && self.code(io, 41, want == pred) {
            return pred;
        }
        let cands: Vec<(i8, i8)> = self.stk_mru.iter().copied().filter(|&s| s != ps && s != pred).take(3).collect();
        for (i, &c) in cands.iter().enumerate() {
            if self.code(io, 42 + i, want == c) {
                return c;
            }
        }
        // zero?
        if self.code(io, 2300 + (ps == (0, 0)) as usize, want == (0, 0)) {
            return (0, 0);
        }
        // the shell: the previous one again, else 8 bits against it
        let (pshell, _) = self.shells.of(ps.0, ps.1);
        let (wshell, widx) = self.shells.of(want.0, want.1);
        let shell = if self.code(io, 2302, wshell == pshell) { pshell } else { self.code_tree_rel(io, 2310, 8, wshell as u32, pshell as u32) as u8 };
        // the place on the shell: an offset in angle order from the predicted direction
        let pd = if self.pred != (0, 0) { self.pred } else { (self.last_nz.0 as i32, self.last_nz.1 as i32) };
        let shells = self.shells;
        let Some(v) = shells.pts.get(shell as usize).filter(|v| !v.is_empty()) else {
            self.bad = true;
            return (0, 0);
        };
        let n = v.len() as i32;
        let anchor = v.partition_point(|p| ang_before((p.0 as i32, p.1 as i32), pd)) as i32 % n;
        let d = (widx as i32 - anchor + n / 2).rem_euclid(n) - n / 2;
        let d = self.code_off(io, 3080, d);
        v[(anchor + d).rem_euclid(n) as usize]
    }

    /// Codes `pad` (encoder) or decodes it (decoder; `pad` ignored) in the state `f`, and makes it the
    /// previous pad.
    fn code_pad<B: BitIo>(&mut self, io: &mut B, f: &Features, pad: GamePad) -> GamePad {
        self.begin(f);
        let prev = self.prev;
        let (mut button, mut sx, mut sy) = (prev.button, prev.stick_x, prev.stick_y);
        if !self.code(io, 0, pad == prev) {
            let bchg = self.code(io, 1, pad.button != prev.button);
            if bchg {
                // recently used buttons first
                let cands: Vec<u16> = self.btn_mru.iter().copied().filter(|&b| b != prev.button).take(3).collect();
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
            self.srun = 0;
            Self::touch_mru(&mut self.stk_mru, (prev.stick_x, prev.stick_y));
        } else {
            self.srun += 1;
        }
        self.run = if out == prev { self.run + 1 } else { 0 };
        self.hist = self.hist << 1 | (out == prev) as u32;
        self.bhist = self.bhist << 1 | (out.button != prev.button) as u32;
        self.shist = self.shist << 1 | ((out.stick_x, out.stick_y) != (prev.stick_x, prev.stick_y)) as u32;
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

/// Codecs 6 (Euclidean shells) and 7 (Chebyshev shells). FROZEN, like every id once it ships.
pub struct StateMix2 {
    pub id: u8,
}

/// 6: polar (best on miner runs); 7: square rings (best on human TASes).
pub static VARIANTS: [StateMix2; 2] = [StateMix2 { id: 6 }, StateMix2 { id: 7 }];

impl StateMix2 {
    fn model(&self) -> Model {
        Model::new(self.id == 7)
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
    body: &'a [u8],
}

impl StateDecoder for MixDecoder<'_> {
    fn pad(&mut self, f: &Features) -> GamePad {
        self.m.code_pad(&mut self.io, f, GamePad::default())
    }
    fn poked(&mut self) {
        self.m.poked();
    }
    fn finish(self: Box<Self>) -> Vec<u8> {
        if self.m.bad {
            // never equal to the body: the run is rejected as not canonical
            let mut v = self.body.to_vec();
            v.push(0);
            return v;
        }
        self.io.e.finish()
    }
}

impl StateCodec for StateMix2 {
    fn id(&self) -> u8 {
        self.id
    }
    fn name(&self) -> &'static str {
        if self.id == 7 { "statemix2-square" } else { "statemix2-polar" }
    }
    fn encoder(&self) -> Box<dyn StateEncoder> {
        Box::new(MixEncoder { m: self.model(), e: RangeEncoder::default() })
    }
    fn decoder<'a>(&self, body: &'a [u8]) -> Box<dyn StateDecoder + 'a> {
        Box::new(MixDecoder { m: self.model(), io: DecEnc { d: RangeDecoder::new(body), e: RangeEncoder::default() }, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::RangeEncoder;

    // Deterministic synthetic runs for the golden vectors: (features, wanted pad, poked).
    fn lcg(s: &mut u64) -> u32 {
        *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (*s >> 33) as u32
    }

    /// kind 0: miner-like (full-tilt sticks near the radius-126 ring, held for runs, camera drifts);
    /// kind 1: human-like (square-edge and small sticks, buttons, fast camera).
    fn synth_run(kind: u32, n: usize, seed: u64) -> Vec<(Features, GamePad, bool)> {
        let mut s = seed;
        let mut pad = GamePad::default();
        let mut cam = 0u16;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let r = lcg(&mut s);
            let change = if kind == 0 { r % 6 == 0 } else { r % 4 == 0 };
            if change {
                let r2 = lcg(&mut s);
                let btn = if r2 % 3 == 0 { 0x8000 } else if r2 % 7 == 0 { 0x4000 } else { 0 };
                if kind == 0 {
                    // a point near the ring of radius 125..127 in a random direction
                    let (mut x, mut y) = ((lcg(&mut s) % 255) as i32 - 127, (lcg(&mut s) % 255) as i32 - 127);
                    if x == 0 && y == 0 { y = 1; }
                    let rad = 125 + (lcg(&mut s) % 3) as i32;
                    let m = ((x * x + y * y) as u32).isqrt() as i32;
                    x = x * rad / m;
                    y = y * rad / m;
                    pad = GamePad::new(btn, x.clamp(-128, 127) as i8, y.clamp(-128, 127) as i8);
                } else {
                    let m = lcg(&mut s);
                    let (x, y) = match m % 4 {
                        0 => (127 * if m & 8 != 0 { 1 } else { -1 }, (lcg(&mut s) % 255) as i32 - 127),
                        1 => ((lcg(&mut s) % 255) as i32 - 127, -128 + 255 * (m as i32 >> 4 & 1)),
                        2 => ((lcg(&mut s) % 41) as i32 - 20, (lcg(&mut s) % 41) as i32 - 20),
                        _ => (0, 0),
                    };
                    pad = GamePad::new(btn, x.clamp(-128, 127) as i8, y.clamp(-128, 127) as i8);
                }
            }
            if kind == 0 { cam = cam.wrapping_add((lcg(&mut s) % 40) as u16); } else { cam = cam.wrapping_add((lcg(&mut s) % 900) as u16 * (r % 2) as u16); }
            let f = Features {
                action: 0x0400_0440 + (i as u32 / 37 % 5) * 3,
                action_timer: (i % 23) as u16,
                fwd: ((i as i32 * 7) % 9000) - 300,
                face_yaw: cam.wrapping_add((lcg(&mut s) % 6000) as u16),
                cam_yaw: cam,
                vel: [(lcg(&mut s) % 4000) as i32 - 2000, (lcg(&mut s) % 3) as i32 - 1, (lcg(&mut s) % 4000) as i32 - 2000],
                pos: [(i as i32) * 13 - 4000, 100, 900 - (i as i32) * 5],
                level: 9,
                area: 1,
                ..Default::default()
            };
            out.push((f, pad, lcg(&mut s) % 13 == 0));
        }
        out
    }

    fn fnv(b: &[u8]) -> u64 {
        b.iter().fold(0xcbf29ce484222325u64, |h, &x| (h ^ x as u64).wrapping_mul(0x100000001b3))
    }


    /// (codec id, kind, frames, seed) -> (bytes, fnv-1a of them), plus the whole encoding for the short runs.
    /// Generated by the lab model (compressor-research-2, smode=2 [,cheb=1]); a change here is a consensus break.
    const GOLDEN: &[(u8, u32, usize, u64, usize, u64, &str)] = &[
        (6, 0, 200, 1, 89, 0x32fc29dd54add65c, "374f1f76ab3780f90ec00763c4ec71280767b4eff3783ccee75af06550cfe653e6a3a4f582075ac501ad277602c7786248c9afc13b3ce99aaaca8535b1d99c8c32599304ae80b89522c6e7156a43df9da846259bdf40501e80"),
        (6, 1, 200, 2, 145, 0x251b1daf86c5544e, "c4f8863d6bd5d179df116084ab2052f16a1829a81c09a492d2e3bd05fc2c9068a782a48ae2d9855b13e3f8b38418beab5b1a9837ddd688a5d58f4d877f56bd353b2e7840ed83cc3f4659d4ab208b43b5cfbd78a49bc80659886ad1d744347795ae7420044e52c1af84315e89144f39ef5be067622ed9f201cd3e834a3c818c87ca6711dc91d093d6dd64c75eafb8c4a180"),
        (6, 0, 6000, 3, 2186, 0x036ccdf7f78bd92f, ""),
        (6, 1, 6000, 4, 3111, 0x767884caa18e3f0f, ""),
        (7, 0, 200, 1, 93, 0x4f8be46daeddb0e5, "374f1f76aaf8246722acad90cfa38c58ce9725231a8c40533a784106e5ea2f42b877392ee3a415182304497c36555e0520ec8083f93586614978a8259ab070314a7734bd9f8c678f407ee035711e5612f06b0283c13185ea733df92a68"),
        (7, 1, 200, 2, 137, 0xa36a490a14ca09ff, "c4f8863d60cfee72ca04aa29595aa32feb3ef674a2a29c268fa12193ee90d3f3e84f19c8b2468ab21e64cdc99242a85fa533b80482b9d1dcb2261e624c0d01fd8e9ad489d94e2089498c90e9bc74f540776ece5fc5ac9c5de9b0d70affff36949f8a6e88f7b50a518bcb205a494c9160e3672fd6ce307990223b851306d0946a3fe5ae37c4f94d46bd"),
        (7, 0, 6000, 3, 2548, 0x23930dd303402a3a, ""),
        (7, 1, 6000, 4, 2806, 0xf5605e890d8fc253, ""),
    ];

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn encode(c: &dyn StateCodec, run: &[(Features, GamePad, bool)]) -> Vec<u8> {
        let mut e = c.encoder();
        for (f, p, poked) in run {
            if *poked { e.poked() } else { e.pad(f, *p) }
        }
        e.finish()
    }

    fn decode(c: &dyn StateCodec, body: &[u8], run: &[(Features, GamePad, bool)]) -> (Vec<GamePad>, Vec<u8>) {
        let mut d = c.decoder(body);
        let mut out = vec![];
        for (f, _, poked) in run {
            if *poked { d.poked() } else { out.push(d.pad(f)) }
        }
        (out, d.finish())
    }

    fn codec(id: u8) -> &'static dyn StateCodec {
        crate::state_codec(id).unwrap()
    }

    #[test]
    fn registered_as_6_and_7() {
        assert_eq!(codec(6).name(), "statemix2-polar");
        assert_eq!(codec(7).name(), "statemix2-square");
        assert_eq!(crate::COMMITTED_ONLY, &[6, 7]);
    }

    #[test]
    fn golden_vectors_codec_6_and_7() {
        for &(id, kind, n, seed, len, hash, whole) in GOLDEN {
            let run = synth_run(kind, n, seed);
            let body = encode(codec(id), &run);
            assert_eq!((body.len(), fnv(&body)), (len, hash), "codec {id} kind {kind} n {n}");
            if !whole.is_empty() {
                assert_eq!(hex(&body), whole, "codec {id} kind {kind} n {n}");
            }
        }
    }

    #[test]
    fn codec_6_and_7_round_trip_canonically() {
        for id in [6, 7] {
            for (kind, seed) in [(0, 11), (1, 12), (0, 13), (1, 14)] {
                let run = synth_run(kind, 3000, seed);
                let pads: Vec<GamePad> = run.iter().filter(|r| !r.2).map(|r| r.1).collect();
                let body = encode(codec(id), &run);
                let (got, again) = decode(codec(id), &body, &run);
                assert_eq!(got, pads, "codec {id}");
                assert_eq!(again, body, "canonical");
            }
        }
    }

    /// A quarter of the lattice (a fixed pseudo-random permutation of it, with button changes, pokes and camera
    /// turns in between) survives, and so do the corners and edges.
    #[test]
    fn codec_6_and_7_cover_every_stick() {
        for id in [6, 7] {
            let mut run = vec![];
            let mut s = 99u64;
            for k in 0..16384u32 + 8 {
                let q = if k < 16384 { k.wrapping_mul(4093) % 65536 } else { [0, 255, 255 << 8, 0xFFFF, 128, 128 << 8, 0x80FF, 0xFF80][(k - 16384) as usize] };
                let (x, y) = ((q >> 8) as i32 - 128, (q & 255) as i32 - 128);
                let f = Features { cam_yaw: (k * 31) as u16, ..Default::default() };
                run.push((f, GamePad::new(0, x as i8, y as i8), false));
                if lcg(&mut s) % 3 == 0 {
                    run.push((f, GamePad::new(0x8000, 0, 0), lcg(&mut s) % 5 == 0));
                }
            }
            let pads: Vec<GamePad> = run.iter().filter(|r| !r.2).map(|r| r.1).collect();
            let body = encode(codec(id), &run);
            let (got, again) = decode(codec(id), &body, &run);
            assert_eq!(got, pads);
            assert_eq!(again, body);
        }
    }

    /// A corrupt body never panics. (Most garbage is the valid encoding of some other run, which the
    /// committed input hash then rejects; a shell without points is flagged `bad` and can never be canonical.)
    #[test]
    fn corrupt_bodies_do_not_panic() {
        let mut flagged = 0;
        for id in [6, 7] {
            let run = synth_run(1, 600, 5);
            let body = encode(codec(id), &run);
            let mut s = 7u64;
            for round in 0..300 {
                let mut bad = body.clone();
                match round % 3 {
                    0 => {
                        let i = lcg(&mut s) as usize % bad.len();
                        bad[i] ^= 1 << (lcg(&mut s) % 8);
                    }
                    1 => bad.truncate(lcg(&mut s) as usize % bad.len()),
                    _ => bad = (0..bad.len()).map(|_| lcg(&mut s) as u8).collect(),
                }
                let (_, again) = decode(codec(id), &bad, &run);
                if again.len() == bad.len() + 1 && again[..bad.len()] == bad[..] {
                    flagged += 1;
                    assert_ne!(again, bad);
                }
            }
        }
        eprintln!("{flagged} of 600 corrupt bodies hit an empty shell");
    }

    #[test]
    fn square_shells_and_polar_shells_partition_the_lattice() {
        for square in [false, true] {
            let sh = shells(square);
            assert_eq!(sh.pts.iter().map(|v| v.len()).sum::<usize>(), 65536);
            for (k, v) in sh.pts.iter().enumerate() {
                for (i, &(x, y)) in v.iter().enumerate() {
                    assert_eq!(sh.of(x, y), (k as u8, i as u16));
                }
            }
            assert_eq!(sh.pts[0], vec![(0, 0)]);
        }
        assert_eq!(shells(true).pts[127].len(), 8 * 127 + 0);
        let _ = RangeEncoder::default();
    }
}
