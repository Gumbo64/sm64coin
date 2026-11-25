//! The perturbation's seed (layer 1).
//!
//! A block's seed is all 256 bits of `cn_fast_hash(block hashing blob)` with
//! the block's timestamp zeroed (the daemon computes it:
//! `Blockchain::sm64_seed`), so a miner can restamp a block without changing
//! its run, while every other field of the header (parent, miner tx, nonce,
//! ...) changes the pokes. The perturbation seeds its generators from all
//! 32 bytes ([`sm64coin_perturb::Perturber::new`]); [`Seed::word`] and
//! [`seed_from_hash`] are a 32-bit summary for display (RPC `sm64_seed`).
//!
//! Read next: rules.rs.

use anyhow::Result;

/// The seed's first word ([`Seed::word`]`(0)`: the first four bytes, little
/// endian, zero becoming one), for display (RPC `sm64_seed`). The
/// perturbation uses the whole hash.
pub fn seed_from_hash(hash: &[u8; 32]) -> u32 {
    Seed(*hash).word(0)
}

/// A block's perturbation seed: `cn_fast_hash(block hashing blob)`, the
/// timestamp zeroed. All 32 bytes seed the perturbation's generators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seed(pub [u8; 32]);

impl Seed {
    /// Word `k`: 32 bits of the hash (cycling through its 8 words), mixed
    /// with `k` so later cycles differ. Never zero. (A leftover of the old
    /// xorshift32 perturbation, which reseeded from word `k` every
    /// `reseed_frames`; now only [`seed_from_hash`] uses it.)
    pub fn word(&self, k: u32) -> u32 {
        let i = (k as usize % 8) * 4;
        let w = u32::from_le_bytes([self.0[i], self.0[i + 1], self.0[i + 2], self.0[i + 3]]);
        let w = w ^ (k / 8).wrapping_mul(0x9E37_79B9);
        if w == 0 { 1 } else { w }
    }

    pub fn from_hex(h: &str) -> Result<Seed> {
        let mut out = [0u8; 32];
        if h.len() != 64 {
            anyhow::bail!("seed hash must be 64 hex characters");
        }
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&h[2 * i..2 * i + 2], 16)?;
        }
        Ok(Seed(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_words_cover_the_hash() {
        let mut h = [0u8; 32];
        for (i, b) in h.iter_mut().enumerate() {
            *b = i as u8 + 1;
        }
        let s = Seed(h);
        let words: Vec<u32> = (0..8).map(|k| s.word(k)).collect();
        let mut d = words.clone();
        d.dedup();
        assert_eq!(d.len(), 8);
        assert_ne!(s.word(0), s.word(8), "later cycles differ");
        assert_eq!(Seed([0; 32]).word(0), 1);
        let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(Seed::from_hex(&hex).unwrap(), s);
    }

    #[test]
    fn seed_never_zero() {
        assert_eq!(seed_from_hash(&[0; 32]), 1);
        let mut h = [0u8; 32];
        h[0] = 7;
        assert_eq!(seed_from_hash(&h), 7);
    }
}
