//! Proof payouts (v0.4, layer 1, pure): the coinbase output that pays an included proof, derived
//! the standard Monero way from the proof's committed payout address and a **public**,
//! deterministic per-proof tx key, so every node can check it and an unmodified wallet finds it.
//! docs/V0.4-DESIGN.md section 3.2.
//!
//! ```text
//! r_j  = H_s("sm64coin proof payout v1" || leaf)                (public)
//! R_j  = r_j G  (standard address)   or   r_j D  (subaddress, D its spend key)   -> additional tx pubkey j
//! d    = 8 r_j A                                                 (A: the address's view key)
//! P_j  = H_s(d || varint(j)) G + B                               (B: the spend key)
//! tag  = keccak256("view_tag" || d || varint(j))[0]
//! ```
//!
//! The primitives are byte-compatible with Monero's `generate_key_derivation`,
//! `derive_public_key` and `derive_view_tag` (tested against `monero/tests/crypto/tests.txt`).
//! Whether the owner adopts this payout scheme is OPEN (design section 7, Q13).

use crate::records::Payout;
use crate::sign::cn_fast_hash;
use curve25519_dalek::constants::ED25519_BASEPOINT_TABLE;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;

const R_TAG: &[u8] = b"sm64coin proof payout v1";

fn point(k: &[u8; 32]) -> Option<EdwardsPoint> {
    CompressedEdwardsY(*k).decompress()
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Monero's `generate_key_derivation(public, secret)`: `8 * secret * public`. None: not a point,
/// or the secret is not a canonical scalar.
pub fn key_derivation(public: &[u8; 32], secret: &[u8; 32]) -> Option<[u8; 32]> {
    let s: Option<Scalar> = Scalar::from_canonical_bytes(*secret).into();
    Some((point(public)? * s?).mul_by_cofactor().compress().to_bytes())
}

/// Monero's `derivation_to_scalar`: `H_s(derivation || varint(index))`.
pub fn derivation_scalar(derivation: &[u8; 32], index: u64) -> Scalar {
    let mut b = derivation.to_vec();
    varint(&mut b, index);
    Scalar::from_bytes_mod_order(cn_fast_hash(&b))
}

/// Monero's `derive_public_key(derivation, index, base)`.
pub fn derive_public_key(derivation: &[u8; 32], index: u64, base: &[u8; 32]) -> Option<[u8; 32]> {
    Some((&derivation_scalar(derivation, index) * ED25519_BASEPOINT_TABLE + point(base)?).compress().to_bytes())
}

/// Monero's `derive_view_tag(derivation, index)`.
pub fn view_tag(derivation: &[u8; 32], index: u64) -> u8 {
    let mut b = b"view_tag".to_vec();
    b.extend_from_slice(derivation);
    varint(&mut b, index);
    cn_fast_hash(&b)[0]
}

/// The public tx key scalar of the output paying the record with `leaf`.
pub fn proof_r(leaf: &[u8; 32]) -> Scalar {
    let mut b = R_TAG.to_vec();
    b.extend_from_slice(leaf);
    Scalar::from_bytes_mod_order(cn_fast_hash(&b))
}

/// The coinbase output (at `index`) that pays the record with `leaf` to `payout`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProofOutput {
    /// The one-time output key `P_j`.
    pub key: [u8; 32],
    pub view_tag: u8,
    /// The additional tx public key `R_j`.
    pub tx_pubkey: [u8; 32],
}

/// Derives the output that pays a proof. None: the payout keys are not points (records.rs refuses
/// such a payout, so this does not happen for a checked record).
pub fn proof_output(leaf: &[u8; 32], payout: &Payout, index: u64) -> Option<ProofOutput> {
    let r = proof_r(leaf);
    let (spend, view) = (point(&payout.spend)?, point(&payout.view)?);
    let tx_pub = if payout.subaddress { spend * r } else { &r * ED25519_BASEPOINT_TABLE };
    let d = (view * r).mul_by_cofactor().compress().to_bytes();
    Some(ProofOutput { key: derive_public_key(&d, index, &payout.spend)?, view_tag: view_tag(&d, index), tx_pubkey: tx_pub.compress().to_bytes() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h32(s: &str) -> [u8; 32] {
        (0..32).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect::<Vec<_>>().try_into().unwrap()
    }

    /// Monero's own crypto test vectors (the fork's `tests/crypto/tests.txt`), when present.
    #[test]
    fn monero_vectors() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../monero/tests/crypto/tests.txt");
        let Ok(t) = std::fs::read_to_string(path) else {
            eprintln!("skipping: no {path}");
            return;
        };
        let mut n = [0usize; 3];
        for l in t.lines() {
            let w: Vec<&str> = l.split_whitespace().collect();
            match w[0] {
                "generate_key_derivation" => {
                    let got = key_derivation(&h32(w[1]), &h32(w[2]));
                    if w[3] == "true" {
                        assert_eq!(got, Some(h32(w[4])), "{l}");
                        n[0] += 1;
                    }
                }
                "derive_public_key" => {
                    let got = derive_public_key(&h32(w[1]), w[2].parse().unwrap(), &h32(w[3]));
                    if w[4] == "true" {
                        assert_eq!(got, Some(h32(w[5])), "{l}");
                        n[1] += 1;
                    }
                }
                "derive_view_tag" => {
                    assert_eq!(view_tag(&h32(w[1]), w[2].parse().unwrap()), u8::from_str_radix(w[3], 16).unwrap(), "{l}");
                    n[2] += 1;
                }
                _ => {}
            }
        }
        assert!(n.iter().all(|&k| k > 50), "vectors checked: {n:?}");
    }

    /// A wallet with view secret a and spend secret b finds the output the way wallet2 does
    /// (derivation from its view key and R_j, then P_j - H_s(d || j) G against its spend key).
    #[test]
    fn a_wallet_finds_its_proof_output() {
        let s = |x: u8| Scalar::from_bytes_mod_order([x; 32]);
        let (a, b) = (s(5), s(6));
        let (cap_a, cap_b) = (&a * ED25519_BASEPOINT_TABLE, &b * ED25519_BASEPOINT_TABLE);
        let leaf = [9u8; 32];
        // Standard address (A, B).
        let p = Payout { subaddress: false, spend: cap_b.compress().to_bytes(), view: cap_a.compress().to_bytes() };
        let o = proof_output(&leaf, &p, 3).unwrap();
        let d = key_derivation(&o.tx_pubkey, a.as_bytes()).unwrap();
        assert_eq!(derive_public_key(&d, 3, &p.spend), Some(o.key));
        assert_eq!(view_tag(&d, 3), o.view_tag);
        assert_ne!(derive_public_key(&d, 4, &p.spend), Some(o.key), "the index is part of it");
        // Subaddress m: D = B + H_s(...) G (any m here), C = a D.
        let m = s(11);
        let dd = cap_b + &m * ED25519_BASEPOINT_TABLE;
        let cc = dd * a;
        let p = Payout { subaddress: true, spend: dd.compress().to_bytes(), view: cc.compress().to_bytes() };
        let o = proof_output(&leaf, &p, 1).unwrap();
        let d = key_derivation(&o.tx_pubkey, a.as_bytes()).unwrap();
        assert_eq!(derive_public_key(&d, 1, &p.spend), Some(o.key), "the wallet's view secret finds a subaddress payout");
        // Deterministic and per-record.
        assert_eq!(proof_output(&leaf, &p, 1), Some(o));
        assert_ne!(proof_output(&[8; 32], &p, 1).unwrap().key, o.key);
    }
}
