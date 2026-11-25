//! The binary range coder (LZMA style) every state codec writes through.
//!
//! 12- and 16-bit probabilities; [`RangeEncoder::finish`] is a canonical
//! minimal flush and the decoder reads an implicit all-zero tail, so each bit
//! sequence has exactly one byte string. [`BitIo`] is the one code path for
//! both directions (the encoder codes a bit, the decoder returns one), so a
//! model can't encode and decode differently. [`CostMeter`] only measures
//! (model development).

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
}
