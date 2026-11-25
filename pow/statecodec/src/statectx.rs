//! Codec 3 (`statectx1`): an adaptive context-mixing model of the pads.
//!
//! Predicts every binary decision of a pad (same as before?, which buttons,
//! which stick values) from small contexts of the game state ([`Features`])
//! and the previous pads; a logistic mixer combines them and every counter
//! adapts while coding (prequential: nothing is transmitted). Still accepted
//! in blocks; sealing now uses codecs 4/5 (statemix.rs).

use crate::codec::{StateCodec, StateDecoder, StateEncoder};
use crate::logistic::{squash, stretch_table};
use crate::range::{BitIo, CostMeter, RangeDecoder, RangeEncoder};
use sm64coin_perturb::Pad as GamePad;
use sm64coin_perturb::sm64::Features;

const INPUTS: usize = 8;
const NODES: usize = 256;
const CTX: usize = 1024;
const LIMIT: u32 = 20;
const NEAR_BITS: u32 = 4;
const NEAR: i32 = 1 << NEAR_BITS;

/// An adaptive probability: P(1) in 16 bits and how many bits it has seen.
#[derive(Clone, Copy)]
struct Counter {
    p: u16,
    n: u16,
}

impl Counter {
    const INIT: Counter = Counter { p: 1 << 15, n: 0 };
    fn update(&mut self, bit: bool, limit: u32) {
        let target = if bit { 65535i32 } else { 0 };
        let rate = 65536 * 2 / (2 * self.n as i32 + 3);
        self.p = (self.p as i32 + (((target - self.p as i32) * rate) >> 16)) as u16;
        if (self.n as u32) < limit {
            self.n += 1;
        }
    }
}

/// Octant-and-length class of a stick: 0 neutral, else 1 + 8 directions * 2.
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
    let long = (ax.max(ay) >= 64) as usize;
    1 + dir * 2 + long
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

/// The context state: the previous wanted pad, the run it has held, how long
/// since the last poke, and the adaptive tables.
pub struct Model {
    stretch: Vec<i16>,
    counters: Vec<Counter>,
    weights: Vec<i32>,
    prev: GamePad,
    run: u32,
    since_poke: u32,
    hist: u32,
    // per-frame contexts (set by `begin`)
    base: [usize; INPUTS],
    sel: [usize; 2],
    pub lr: i32,
    pub limit: u32,
}

impl Default for Model {
    fn default() -> Self {
        Model::new()
    }
}

impl Model {
    pub fn new() -> Model {
        Model {
            stretch: stretch_table(),
            counters: vec![Counter::INIT; INPUTS * NODES * CTX],
            weights: vec![(65536 * 2 / 10) as i32; NODES * 4 * INPUTS],
            prev: GamePad::default(),
            run: 0,
            since_poke: 3,
            hist: 0,
            base: [0; INPUTS],
            sel: [0; 2],
            lr: 20,
            limit: LIMIT,
        }
    }

    /// A copy for coding a new run: keeps the learned tables, but trusts each
    /// counter as only `n0` observations, and forgets the pad history.
    pub fn restart(&self, n0: u16, limit: u32) -> Model {
        let mut m = Model {
            stretch: self.stretch.clone(),
            counters: self.counters.clone(),
            weights: self.weights.clone(),
            prev: GamePad::default(),
            run: 0,
            since_poke: 3,
            hist: 0,
            base: [0; INPUTS],
            sel: [0; 2],
            lr: self.lr,
            limit,
        };
        for c in m.counters.iter_mut() {
            c.n = c.n.min(n0);
        }
        m
    }

    /// A frame the perturbation poked: nothing is coded, but it is noted.
    pub fn poked(&mut self) {
        self.since_poke = 0;
    }

    fn begin(&mut self, f: &Features) {
        let act = (f.action & 0x1FF) as usize;
        let group = ((f.action >> 6) & 7) as usize;
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
        let pk = self.since_poke.min(3) as usize;
        let pcls = stick_class(self.prev.stick_x, self.prev.stick_y);
        let rb = run_bucket(self.run);
        let btn = match self.prev.button {
            0 => 0,
            0x8000 => 1,
            0x4000 => 2,
            0x2000 => 3,
            b if b & 0x8000 != 0 => 4,
            _ => 5,
        };
        let dyaw = (f.face_yaw.wrapping_sub(f.cam_yaw) >> 12) as usize;
        self.base = [
            0,
            act,
            (pcls * 8 + rb) * 6 + btn,
            ((speed * 6 + timer) * 8 + group) * 4 + pk,
            dyaw * 24 + pcls,
            (rb * 4 + pk) * 8 + group,
            ((self.hist & 63) as usize) * 8 + group,
            (((self.prev.stick_x as u8) >> 3) as usize) * 32 + ((self.prev.stick_y as u8) >> 3) as usize,
        ];
        self.sel = [rb, pk];
    }

    /// Codes one binary decision at `node` with the frame's contexts.
    fn code<B: BitIo>(&mut self, io: &mut B, node: usize, bit: bool) -> bool {
        debug_assert!(node < NODES);
        let mut idx = [0usize; INPUTS];
        let mut st = [0i32; INPUTS];
        for i in 0..INPUTS {
            idx[i] = (i * NODES + node) * CTX + self.base[i].min(CTX - 1);
            st[i] = self.stretch[(self.counters[idx[i]].p >> 4) as usize] as i32;
        }
        let wbase = (node * 4 + self.sel[1].min(3)) * INPUTS;
        let mut dot = 0i64;
        for i in 0..INPUTS {
            dot += st[i] as i64 * self.weights[wbase + i] as i64;
        }
        let pm = squash((dot >> 16) as i32).clamp(1, 4095);
        let bit = io.bit(bit, pm as u32);
        let err = ((bit as i32) << 12) - pm;
        for i in 0..INPUTS {
            self.weights[wbase + i] += (st[i] * err * self.lr) >> 14;
            self.counters[idx[i]].update(bit, self.limit);
        }
        bit
    }

    /// Codes a value in `0..2^bits` as a binary tree below `node`..`node + 2^bits`.
    fn code_tree<B: BitIo>(&mut self, io: &mut B, node: usize, bits: u32, v: u32) -> u32 {
        let mut n = 1u32;
        for i in (0..bits).rev() {
            let b = self.code(io, node + n as usize, (v >> i) & 1 != 0);
            n = n * 2 + b as u32;
        }
        n - (1 << bits)
    }

    /// An axis value other than `prev`: first whether it is within 8 of it
    /// (then sign and distance), else zero?, sign, and the magnitude's bit
    /// length in a tree and its mantissa bits.
    fn code_axis<B: BitIo>(&mut self, io: &mut B, axis: usize, prev: i8, v: i8) -> i8 {
        let base = 40 + axis * 108;
        let d = v as i32 - prev as i32;
        let near = self.code(io, base + 90, d.abs() <= NEAR);
        if near {
            let up = self.code(io, base + 91, d > 0);
            let dist = self.code_tree(io, base + 92, NEAR_BITS, (d.abs() - 1).max(0) as u32) as i32 + 1;
            return (prev as i32 + if up { dist } else { -dist }) as i8;
        }
        let zero = self.code(io, base, v == 0);
        if zero {
            return 0;
        }
        let neg = self.code(io, base + 1, v < 0);
        let m = (v as i32).unsigned_abs().min(128); // 1..=128
        let e = 32 - m.leading_zeros(); // 1..=8
        let e = self.code_tree(io, base + 2, 3, e.wrapping_sub(1)) + 1;
        let mut mag = 1u32;
        for i in (0..e - 1).rev() {
            let hi = e - 2 - i; // mantissa bits already coded
            let node = if hi < 2 { base + 12 + e as usize * 4 + (mag as usize & 3) } else { base + 50 + e as usize };
            let b = self.code(io, node, (m >> i) & 1 != 0);
            mag = mag * 2 + b as u32;
        }
        let r = if neg { -(mag as i32) } else { mag as i32 };
        r.clamp(-128, 127) as i8
    }

    /// Codes `pad` (encoder) or decodes it (decoder; `pad` ignored) in the
    /// state `f`, and makes it the previous pad.
    pub fn code_pad<B: BitIo>(&mut self, io: &mut B, f: &Features, pad: GamePad) -> GamePad {
        self.begin(f);
        let prev = self.prev;
        let same = self.code(io, 0, pad == prev);
        let out = if same {
            prev
        } else {
            let bchg = self.code(io, 1, pad.button != prev.button);
            let mut button = prev.button;
            if bchg {
                button = 0;
                for i in 0..16 {
                    let pb = (prev.button >> i) & 1 != 0;
                    let node = 2 + i * 2 + pb as usize; // 2..34
                    let b = self.code(io, node, (pad.button >> i) & 1 != 0);
                    button |= (b as u16) << i;
                }
            }
            let stick_differs = if bchg { self.code(io, 34, (pad.stick_x, pad.stick_y) != (prev.stick_x, prev.stick_y)) } else { true };
            let (mut sx, mut sy) = (prev.stick_x, prev.stick_y);
            if stick_differs {
                let xc = self.code(io, 35, pad.stick_x != prev.stick_x);
                if xc {
                    sx = self.code_axis(io, 0, prev.stick_x, pad.stick_x);
                }
                let yc = if !xc && !bchg || !xc && bchg && false { true } else { self.code(io, 36, pad.stick_y != prev.stick_y) };
                if yc {
                    sy = self.code_axis(io, 1, prev.stick_y, pad.stick_y);
                }
            }
            GamePad::new(button, sx, sy)
        };
        if out == prev {
            self.run += 1;
        } else {
            self.run = 0;
        }
        self.hist = self.hist << 1 | (out == prev) as u32;
        self.prev = out;
        self.since_poke = self.since_poke.saturating_add(1);
        out
    }
}

/// One frame of a run as the codec sees it.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub features: Features,
    pub pad: GamePad,
    pub poked: bool,
}

/// Encodes the non-poked pads of `frames` (the whole stream, no header).
pub fn encode_frames(frames: &[Frame]) -> Vec<u8> {
    encode_frames_with(Model::new(), frames)
}

pub fn encode_frames_with(mut m: Model, frames: &[Frame]) -> Vec<u8> {
    let mut e = RangeEncoder::default();
    for f in frames {
        if f.poked {
            m.poked();
        } else {
            m.code_pad(&mut e, &f.features, f.pad);
        }
    }
    e.finish()
}

/// Decodes `frames`' non-poked pads from `body`, given the features and poke
/// pattern.
pub fn decode_frames(body: &[u8], frames: &[Frame]) -> Vec<GamePad> {
    decode_frames_with(Model::new(), body, frames)
}

pub fn decode_frames_with(mut m: Model, body: &[u8], frames: &[Frame]) -> Vec<GamePad> {
    let mut d = RangeDecoder::new(body);
    let mut out = Vec::new();
    for f in frames {
        if f.poked {
            m.poked();
        } else {
            out.push(m.code_pad(&mut d, &f.features, GamePad::default()));
        }
    }
    out
}

/// Trains a model by coding `runs` one after another (cost metered).
pub fn train(runs: &[Vec<Frame>], limit: u32) -> Model {
    let mut m = Model::new();
    m.limit = limit;
    for run in runs {
        m.prev = GamePad::default();
        m.run = 0;
        let mut io = CostMeter::default();
        for f in run {
            if f.poked {
                m.poked();
            } else {
                m.code_pad(&mut io, &f.features, f.pad);
            }
        }
    }
    m
}

/// Codec 3: the adaptive context-mixing model above, cold start (no trained
/// tables: only the constants in this file).
pub struct StateCtx1;

struct ModelEncoder {
    m: Model,
    e: RangeEncoder,
}

struct ModelDecoder<'a> {
    m: Model,
    d: RangeDecoder<'a>,
    re: ModelEncoder,
}

impl StateEncoder for ModelEncoder {
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

impl StateDecoder for ModelDecoder<'_> {
    fn pad(&mut self, f: &Features) -> GamePad {
        let p = self.m.code_pad(&mut self.d, f, GamePad::default());
        self.re.pad(f, p);
        p
    }
    fn poked(&mut self) {
        self.m.poked();
        self.re.poked();
    }
    fn finish(self: Box<Self>) -> Vec<u8> {
        Box::new(self.re).finish()
    }
}

impl StateCodec for StateCtx1 {
    fn id(&self) -> u8 {
        3
    }
    fn name(&self) -> &'static str {
        "statectx1"
    }
    fn encoder(&self) -> Box<dyn StateEncoder> {
        Box::new(ModelEncoder { m: Model::new(), e: RangeEncoder::default() })
    }
    fn decoder<'a>(&self, body: &'a [u8]) -> Box<dyn StateDecoder + 'a> {
        Box::new(ModelDecoder { m: Model::new(), d: RangeDecoder::new(body), re: ModelEncoder { m: Model::new(), e: RangeEncoder::default() } })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(s: &mut u64) -> u32 {
        *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (*s >> 33) as u32
    }

    fn synth(n: usize, seed: u64) -> Vec<Frame> {
        let mut s = seed;
        let mut pad = GamePad::default();
        (0..n)
            .map(|i| {
                let r = lcg(&mut s);
                if r % 5 == 0 {
                    pad = GamePad::new(if r % 3 == 0 { 0x8000 } else { 0 }, ((lcg(&mut s) % 161) as i32 - 80) as i8, ((lcg(&mut s) % 161) as i32 - 80) as i8);
                }
                let f = Features { action: 0x0C40_0201 + (i as u32 / 50 % 4), fwd: (i as i32 % 40) << 8, ..Default::default() };
                Frame { features: f, pad, poked: lcg(&mut s) % 11 == 0 }
            })
            .collect()
    }

    #[test]
    fn model_round_trips_synthetic_frames() {
        for seed in 1..6 {
            let frames = synth(1500, seed);
            let pads: Vec<GamePad> = frames.iter().filter(|f| !f.poked).map(|f| f.pad).collect();
            let enc = encode_frames(&frames);
            assert_eq!(decode_frames(&enc, &frames), pads);
            assert!(enc.len() < pads.len() * 4 / 4, "compresses");
        }
    }
}
