//! State-aware proof codecs: the inputs are coded given the game state they
//! are played in, so decoding happens frame by frame during the replay.
//!
//! Everything on the decode path is integer arithmetic (no floats, no ML
//! runtime), so every node decodes bit-identically:
//!
//! * [`RangeEncoder`] / [`RangeDecoder`]: a binary range coder (LZMA style,
//!   12-bit probabilities) with a canonical minimal flush and an implicit
//!   all-zero tail, so each bit sequence has exactly one byte string.
//! * [`Model`]: predicts every binary decision of a pad (same as before?,
//!   which buttons, which stick values) from several small contexts of the
//!   game state ([`Features`]) and the previous pads; the predictions are
//!   mixed by a logistic mixer and every counter adapts while coding
//!   (prequential, nothing is transmitted). One code path ([`BitIo`]) both
//!   encodes and decodes, so the two cannot disagree.
//! * [`StateCodec`]: the codec ids built from it. A model change is a new id.
//!
//! This crate is the pure part (no game, no emulator): native nodes use it through
//! `sm64pow::statecodec`, the browser through `sm64coin-perturb-wasm`. Same source, so the
//! two decode identically. Consensus-critical: a change here is a new codec id.

pub mod statemix;
mod trig;

pub use sm64coin_perturb::sm64::Features;
/// A controller pad (buttons and stick).
pub use sm64coin_perturb::Pad;
pub use sm64coin_perturb::Pad as GamePad;


const PROB_BITS: u32 = 12;
const TOP: u32 = 1 << 24;

pub struct RangeEncoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    out: Vec<u8>,
}

impl Default for RangeEncoder {
    fn default() -> Self {
        RangeEncoder { low: 0, range: u32::MAX, cache: 0, cache_size: 1, out: Vec::new() }
    }
}

impl RangeEncoder {
    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut temp = self.cache;
            loop {
                self.out.push(temp.wrapping_add(carry));
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    /// Codes `bit` where `p1` is P(bit = 1) in 1..4096 (12 bits).
    pub fn encode(&mut self, bit: bool, p1: u32) {
        let bound = (self.range >> PROB_BITS) * p1;
        if bit {
            self.range = bound;
        } else {
            self.low += bound as u64;
            self.range -= bound;
        }
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
    }

    /// [`encode`](RangeEncoder::encode) with a 16-bit probability `p1` in 1..65536.
    pub fn encode16(&mut self, bit: bool, p1: u32) {
        let bound = (self.range >> 16) * p1;
        if bit {
            self.range = bound;
        } else {
            self.low += bound as u64;
            self.range -= bound;
        }
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
    }

    /// Ends the stream: the value in the final interval with the most
    /// trailing zero bits, then every trailing zero byte dropped (the decoder
    /// reads zeros past the end). The leading byte is always 0 and is dropped.
    pub fn finish(mut self) -> Vec<u8> {
        let hi = self.low + self.range as u64;
        for k in (0..=32).rev() {
            let mask = (1u64 << k) - 1;
            let v = (self.low + mask) & !mask;
            if v < hi {
                self.low = v;
                break;
            }
        }
        for _ in 0..5 {
            self.shift_low();
        }
        let mut out = self.out.split_off(1);
        while out.last() == Some(&0) {
            out.pop();
        }
        out
    }
}

pub struct RangeDecoder<'a> {
    code: u32,
    range: u32,
    data: &'a [u8],
    pos: usize,
}

impl<'a> RangeDecoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut d = RangeDecoder { code: 0, range: u32::MAX, data, pos: 0 };
        for _ in 0..4 {
            d.code = (d.code << 8) | d.next() as u32;
        }
        d
    }
    fn next(&mut self) -> u8 {
        let b = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }
    pub fn decode16(&mut self, p1: u32) -> bool {
        let bound = (self.range >> 16) * p1;
        let bit = if self.code < bound {
            self.range = bound;
            true
        } else {
            self.code -= bound;
            self.range -= bound;
            false
        };
        while self.range < TOP {
            self.code = (self.code << 8) | self.next() as u32;
            self.range <<= 8;
        }
        bit
    }
    pub fn decode(&mut self, p1: u32) -> bool {
        let bound = (self.range >> PROB_BITS) * p1;
        let bit = if self.code < bound {
            self.range = bound;
            true
        } else {
            self.code -= bound;
            self.range -= bound;
            false
        };
        while self.range < TOP {
            self.code = (self.code << 8) | self.next() as u32;
            self.range <<= 8;
        }
        bit
    }
}

/// One bit of a stream, either way: the encoder codes `bit`, the decoder
/// ignores it and returns what it decodes.
pub trait BitIo {
    fn bit(&mut self, bit: bool, p1: u32) -> bool;
    /// The same with a 16-bit probability (codecs from 4 on).
    fn bit16(&mut self, bit: bool, p1: u32) -> bool {
        self.bit(bit, (p1 >> 4).clamp(1, 4095))
    }
}

impl BitIo for RangeEncoder {
    fn bit(&mut self, bit: bool, p1: u32) -> bool {
        self.encode(bit, p1);
        bit
    }
    fn bit16(&mut self, bit: bool, p1: u32) -> bool {
        self.encode16(bit, p1);
        bit
    }
}

impl BitIo for RangeDecoder<'_> {
    fn bit(&mut self, _: bool, p1: u32) -> bool {
        self.decode(p1)
    }
    fn bit16(&mut self, _: bool, p1: u32) -> bool {
        self.decode16(p1)
    }
}

/// Counts the bits a perfect coder would spend (for model development).
#[derive(Default)]
pub struct CostMeter {
    pub bits: f64,
}

impl BitIo for CostMeter {
    fn bit16(&mut self, bit: bool, p1: u32) -> bool {
        let p = if bit { p1 } else { 65536 - p1 } as f64 / 65536.0;
        self.bits -= p.log2();
        bit
    }
    fn bit(&mut self, bit: bool, p1: u32) -> bool {
        let p = if bit { p1 } else { 4096 - p1 } as f64 / 4096.0;
        self.bits -= p.log2();
        bit
    }
}

// ---------------------------------------------------------------------- model

const SQUASH_T: [i32; 33] = [
    1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549, 2994, 3348, 3607, 3785, 3901, 3975, 4022, 4050, 4068,
    4079, 4085, 4089, 4092, 4093, 4094,
];

/// 4096 / (1 + e^-d/256), d in -2047..=2047, by interpolation.
pub(crate) fn squash(d: i32) -> i32 {
    if d > 2047 {
        return 4095;
    }
    if d < -2047 {
        return 1;
    }
    let w = d & 127;
    let i = ((d >> 7) + 16) as usize;
    (SQUASH_T[i] * (128 - w) + SQUASH_T[i + 1] * w + 64) >> 7
}

pub(crate) fn stretch_table() -> Vec<i16> {
    let mut t = vec![0i16; 4096];
    let mut pi = 0usize;
    for x in -2047..=2047 {
        let v = squash(x) as usize;
        for e in t.iter_mut().take(v + 1).skip(pi) {
            *e = x as i16;
        }
        pi = pi.max(v + 1);
    }
    for e in t.iter_mut().skip(pi) {
        *e = 2047;
    }
    t
}

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

// ---------------------------------------------------------------- the codecs

/// Streaming encoder: the replay hands it every frame in order.
pub trait StateEncoder {
    /// The wanted input of a non-poked frame, in the state `f`.
    fn pad(&mut self, f: &Features, wanted: GamePad);
    /// The frame was poked (no input is stored).
    fn poked(&mut self);
    fn finish(self: Box<Self>) -> Vec<u8>;
}

/// Streaming decoder, the mirror of [`StateEncoder`].
pub trait StateDecoder {
    fn pad(&mut self, f: &Features) -> GamePad;
    fn poked(&mut self);
    /// The canonical encoding of everything decoded so far (the decoder
    /// re-encodes alongside), to compare with the body: a run has exactly one
    /// valid encoding.
    fn finish(self: Box<Self>) -> Vec<u8>;
}

/// A codec whose body is decoded frame by frame during the replay. Like
/// `sm64pow::proof::Codec`, a new model is a new id.
pub trait StateCodec: Sync {
    fn id(&self) -> u8;
    fn name(&self) -> &'static str;
    fn encoder(&self) -> Box<dyn StateEncoder>;
    fn decoder<'a>(&self, body: &'a [u8]) -> Box<dyn StateDecoder + 'a>;
}

/// Every state codec this build knows (accepted in blocks only when
/// [`state_codec_enabled`]).
pub static STATE_CODECS: &[&dyn StateCodec] = &[&StateCtx1, &crate::statemix::VARIANTS[0], &crate::statemix::VARIANTS[1]];

/// The codecs sealing tries (the newest family; 3 stays decodable).
pub static SEAL_CODECS: &[&dyn StateCodec] = &[&crate::statemix::VARIANTS[0], &crate::statemix::VARIANTS[1]];

pub fn state_codec(id: u8) -> Option<&'static dyn StateCodec> {
    STATE_CODECS.iter().copied().find(|c| c.id() == id)
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

/// The proof blob (`varint(frames) || start || id || body`) of `pr` in a
/// state codec: replays the run from `parent` (the parent's end state, for a
/// parent start) to code each input in its state.
#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(s: &mut u64) -> u32 {
        *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (*s >> 33) as u32
    }

    #[test]
    fn range_coder_round_trips_and_is_canonical() {
        let mut s = 7u64;
        for n in [0usize, 1, 2, 5, 100, 5000] {
            let bits: Vec<(bool, u32)> = (0..n).map(|_| (lcg(&mut s) % 7 == 0, 1 + lcg(&mut s) % 4095)).collect();
            let mut e = RangeEncoder::default();
            for &(b, p) in &bits {
                e.encode(b, p);
            }
            let bytes = e.finish();
            assert!(bytes.last() != Some(&0), "no trailing zero byte");
            let mut d = RangeDecoder::new(&bytes);
            for &(b, p) in &bits {
                assert_eq!(d.decode(p), b);
            }
            // The same bits always give the same bytes; stray trailing zeros decode the same.
            let mut e2 = RangeEncoder::default();
            for &(b, p) in &bits {
                e2.encode(b, p);
            }
            assert_eq!(e2.finish(), bytes);
        }
    }

    #[test]
    fn f32_fixed_point_is_exact() {
        use sm64coin_perturb::sm64::f32_fx;
        for (f, want) in [(0.0f32, 0), (1.0, 256), (-1.5, -384), (100.25, 25664), (0.001, 0), (1e30, i32::MAX), (-1e30, -i32::MAX), (f32::NAN, i32::MAX)] {
            assert_eq!(f32_fx(f.to_bits(), 8), want, "{f}");
        }
    }

    fn synth(n: usize, seed: u64) -> Vec<Frame> {
        let mut s = seed;
        let mut pad = GamePad::default();
        (0..n)
            .map(|i| {
                let r = lcg(&mut s);
                if r % 5 == 0 {
                    pad = GamePad::new(if r % 3 == 0 { 0x8000 } else { 0 }, (lcg(&mut s) % 161) as i8 - 80, (lcg(&mut s) % 161) as i8 - 80);
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

