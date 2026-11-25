//! Codec 4 family ("statemix"): a stronger state-aware input model.
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
//! are frozen presets ([`StateMix::cfg`]), the codec id says which one.

use crate::codec::{StateCodec, StateDecoder, StateEncoder};
use crate::logistic::{squash, stretch_table};
use crate::range::{BitIo, RangeDecoder, RangeEncoder};
use crate::trig::SIN;
use sm64coin_perturb::Pad as GamePad;
use sm64coin_perturb::sm64::Features;

const NCTX: usize = 14;
const NODES: usize = 384;
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
}

impl Default for Cfg {
    fn default() -> Cfg {
        Cfg { two_rate: true, rot: 2, mru: false, apm: true, mixers: 2, ctxmask: 0xFFFF, lr: 14, lr2: 6, limit: 120, fast_shift: 4, apm_rate: 6, bias_init: true, trace: false, pm: 0, circle: true, stk_mru: false, lite: false }
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

fn isqrt(n: i32) -> i32 {
    (n.max(0) as u32).isqrt() as i32
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
}

impl Model {
    pub fn new(cfg: Cfg) -> Model {
        let sets1 = NODES * if cfg.mixers > 2 { 37 } else { 9 };
        let w1 = vec![if cfg.two_rate { 65536 * 15 / 100 } else { 65536 * 3 / 10 }; sets1 * NIN_MAX];
        let w2 = vec![65536 / MIXERS as i32; NODES * MIXERS];
        Model {
            probe: Vec::new(),
            apm1: Apm::new(NODES * 8, cfg.apm_rate),
            apm2: Apm::new(1 << 12, cfg.apm_rate),
            cfg,
            stretch: stretch_table(),
            rates: (0..=255).map(|n| 65536 * 2 / (2 * n + 3)).collect(),
            slots: vec![SLOT0; 1 << TABLE_BITS],
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
        }
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
            0,
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
            0,
            0,
            ((vy * 16 + dyaw) * 8 + group) * 6 + btn,
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
        const ALL: [usize; NCTX] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13];
        const LITE: [usize; 6] = [0, 1, 2, 4, 8, 10];
        let lite = self.cfg.lite && node >= 48;
        let ids: &[usize] = if lite { &LITE } else { &ALL };
        let nctx = ids.len();
        for (j, &i) in ids.iter().enumerate() {
            let ix = ((self.ctxh[i] ^ nh).wrapping_mul(0x9E37_79B1) >> (32 - TABLE_BITS)) as usize;
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

    /// Codes `pad` (encoder) or decodes it (decoder; `pad` ignored) in the
    /// state `f`, and makes it the previous pad.
    pub fn code_pad<B: BitIo>(&mut self, io: &mut B, f: &Features, pad: GamePad) -> GamePad {
        self.begin(f);
        let prev = self.prev;
        let same = self.code(io, 0, pad == prev);
        let (mut button, mut sx, mut sy) = (prev.button, prev.stick_x, prev.stick_y);
        if !same {
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
        self.prev = out;
        self.cam_at_prev = f.cam_yaw;
        self.since_poke = self.since_poke.saturating_add(1);
        out
    }
}

// ---------------------------------------------------------------- the codecs

/// Codecs 4..: the model above, cold or with a frozen prior, a variant = one
/// frozen [`Cfg`]. The encoder tries all and keeps the smallest (the id is
/// the signalled choice).
pub struct StateMix {
    pub id: u8,
}

/// 4: the plain model; 5: with the recently-used button and stick lists
/// (better when a run revisits the same pads; costs nothing to try).
pub static VARIANTS: [StateMix; 2] = [StateMix { id: 4 }, StateMix { id: 5 }];

impl StateMix {
    pub fn cfg(&self) -> Cfg {
        let mut c = Cfg::default();
        if self.id == 5 {
            c.mru = true;
            c.stk_mru = true;
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

impl StateCodec for StateMix {
    fn id(&self) -> u8 {
        self.id
    }
    fn name(&self) -> &'static str {
        "statemix"
    }
    fn encoder(&self) -> Box<dyn StateEncoder> {
        Box::new(MixEncoder { m: self.model(), e: RangeEncoder::default() })
    }
    fn decoder<'a>(&self, body: &'a [u8]) -> Box<dyn StateDecoder + 'a> {
        Box::new(MixDecoder { m: self.model(), io: DecEnc { d: RangeDecoder::new(body), e: RangeEncoder::default() } })
    }
}
