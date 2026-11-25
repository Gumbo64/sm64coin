//! The VRF ticket for the final tie key (`pof::TieKey::Vrf`; docs/V0.4-DESIGN.md section 1.6).
//! Designed and tested, NOT the default: the maintainer decides.
//!
//! # Why
//!
//! Near the difficulty floor most heights are exact ties (equal frames, equal canonical size),
//! and the final tie key decides. The signature hash can be ground by replaying equal-frame
//! variants, the seed hash by hashing template keys; both favour farms. A VRF ticket cannot be
//! ground: `VRF(sk, parent_id)` is fixed by the key and the parent, there is exactly one valid
//! output per (key, input), and nothing the run's owner can vary changes it.
//!
//! # The scheme: ECVRF-EDWARDS25519-SHA512-TAI (RFC 9381)
//!
//! Suite string 0x03, cofactor 8, `encode_to_curve` by try-and-increment with salt = the public
//! key, challenge 16 bytes, little-endian integers, SHA-512. The input `alpha` is the parent block
//! id (32 bytes). The proof is `pi = gamma(32) || c(16) || s(32)` (80 bytes); the output is
//! `beta = SHA512(0x03 || 0x03 || 8*gamma || 0x00)` (64 bytes), of which the tie key takes the
//! first 31 bytes (see [`ticket_key`]).
//!
//! **Key adaptation.** RFC 9381 derives its secret scalar from a 32-byte seed: `x = clamp(SHA512(seed)[..32])`
//! and the nonce prefix from `SHA512(seed)[32..]`. The miner's key (sign.rs) is already a scalar `x`
//! (Monero style, `public = x*B`) with no seed, so [`prove`] takes `x` directly and derives the
//! nonce prefix as `SHA512("sm64coin vrf nonce v1" || x)[..32]`. Everything else is the RFC.
//! The core ([`prove_with`]) takes the prefix as a parameter, so the RFC's own vectors (derived
//! scalar and prefix) run through the same code: they pass (tests).
//! The nonce only has to be unpredictable to others; it never affects the output.
//!
//! **Uniqueness (why the output cannot be shopped).** `verify` accepts `(gamma, c, s)` only if the
//! Chaum-Pedersen proof of `log_B(Y) = log_H(gamma)` checks with the Fiat-Shamir challenge, where
//! `H = encode_to_curve(Y, alpha)` is a deterministic function of the key and the input, and `Y`
//! is a canonical, torsion-free point of large order (small-order and non-canonical keys are
//! rejected, stricter than RFC 9381's `validate_key`; so is a gamma that is not canonical and
//! torsion-free, which also keeps the proof bytes themselves canonical). By soundness of the DLEQ
//! proof in the random-oracle model only `gamma = x*H` verifies (finding another is a break of
//! discrete log or of SHA-512 as a random oracle); `beta` is a hash of `8*gamma`. So for one
//! `(Y, alpha)` there is one valid `beta`; the proof bytes `(c, s)` may differ (different nonces),
//! the output does not.
//!
//! **Why not a Schnorr signature as the VRF.** A deterministic-nonce Schnorr signature
//! (sign.rs's, or Ed25519's) is not unique: verification only checks `R = sG - cY` with `c = H(R, ..)`,
//! and any nonce `k` gives a different valid `(c, s)` over the same message. Deterministic nonces
//! make an honest signer's output repeatable, not the *verifier's* acceptance set a single point:
//! the key owner can grind `k` and pick the signature whose hash is lowest, which is exactly the
//! ticket grinding we want to rule out. The VRF's output `x*H` is not a function of the nonce.
//!
//! Cost: one VRF prove per template (two fixed-base-like scalar mults plus the hash-to-curve, about
//! one signature), verify about two double-scalar mults (a signature check is one).
//!
//! # Registration and the rule
//!
//! A key may win a tie with a ticket only if it is *registered*: a block or an included proof
//! record signed by it exists at height `<= h - 2` in the same chain ([`Registry`]); otherwise a
//! fresh key (1e5-1e6 per second per core) reopens grinding. [`ticket_key`] turns (valid VRF
//! output, registered) into the 32-byte final key: registered tickets start with 0x00 and sort
//! before everything else; the fallback is the signature hash with a 0x01 prefix. The fallback
//! class keeps the order total and transitive. [`beats`] is the same rule written as a
//! two-sibling decision.

use crate::sign::{self, Keypair};
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use sha2::{Digest, Sha512};
use std::collections::{BTreeSet, HashMap};

const SUITE: u8 = 0x03;
const NONCE_DOMAIN: &[u8] = b"sm64coin vrf nonce v1";

/// `gamma(32) || c(16) || s(32)`.
pub const PROOF_LEN: usize = 80;
pub type Proof = [u8; PROOF_LEN];
/// `beta`, 64 bytes.
pub type Output = [u8; 64];

fn sha512(parts: &[&[u8]]) -> [u8; 64] {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// RFC 8032 point decoding: canonical y, on the curve.
fn decode(b: &[u8; 32]) -> Option<EdwardsPoint> {
    let p = CompressedEdwardsY(*b).decompress()?;
    (p.compress().to_bytes() == *b).then_some(p)
}

/// A public key or gamma: canonical, not small order, torsion-free.
fn strict_point(b: &[u8; 32]) -> Option<EdwardsPoint> {
    let p = decode(b)?;
    (!p.is_small_order() && p.is_torsion_free()).then_some(p)
}

/// `ECVRF_encode_to_curve` (try and increment, RFC 9381 section 5.4.1.1), salt = the key's bytes.
fn encode_to_curve(key: &[u8; 32], alpha: &[u8]) -> EdwardsPoint {
    for ctr in 0u8..=255 {
        let h = sha512(&[&[SUITE, 0x01], key, alpha, &[ctr, 0x00]]);
        if let Some(p) = decode(h[..32].try_into().unwrap()) {
            let p = p.mul_by_cofactor();
            if p != EdwardsPoint::default() {
                return p;
            }
        }
    }
    // 256 failures happen with probability about 2^-256 per input.
    unreachable!("try-and-increment exhausted")
}

fn challenge(pts: [&EdwardsPoint; 5]) -> [u8; 16] {
    let mut h = Sha512::new();
    h.update([SUITE, 0x02]);
    for p in pts {
        h.update(p.compress().as_bytes());
    }
    h.update([0x00]);
    let d: [u8; 64] = h.finalize().into();
    d[..16].try_into().unwrap()
}

fn c_scalar(c: &[u8; 16]) -> Scalar {
    let mut b = [0u8; 32];
    b[..16].copy_from_slice(c);
    Scalar::from_bytes_mod_order(b)
}

/// RFC 9381's prove with an explicit scalar and nonce prefix (the RFC's `x` and
/// `SHA512(SK)[32..]`; the miner's [`prove`] derives the prefix from its scalar). `key` must be
/// `x*B`. Returns the proof.
pub fn prove_with(x: &Scalar, key: &[u8; 32], nonce_prefix: &[u8; 32], alpha: &[u8]) -> Proof {
    let h = encode_to_curve(key, alpha);
    let gamma = x * h;
    let k_wide = sha512(&[nonce_prefix, h.compress().as_bytes()]);
    let k = Scalar::from_bytes_mod_order_wide(&k_wide);
    let u = &k * curve25519_dalek::constants::ED25519_BASEPOINT_TABLE;
    let v = k * h;
    let y = decode(key).expect("own key");
    let c = challenge([&y, &h, &gamma, &u, &v]);
    let s = k + c_scalar(&c) * x;
    let mut pi = [0u8; PROOF_LEN];
    pi[..32].copy_from_slice(gamma.compress().as_bytes());
    pi[32..48].copy_from_slice(&c);
    pi[48..].copy_from_slice(s.as_bytes());
    pi
}

/// The VRF proof of `alpha` (the parent block id) under the miner's key.
pub fn prove(kp: &Keypair, alpha: &[u8]) -> Proof {
    let x = Scalar::from_canonical_bytes(kp.secret).unwrap();
    let prefix: [u8; 32] = sha512(&[NONCE_DOMAIN, &kp.secret])[..32].try_into().unwrap();
    prove_with(&x, &kp.public, &prefix, alpha)
}

/// The output of a proof (`proof_to_hash`), from its gamma. None for a malformed gamma.
pub fn proof_to_hash(pi: &Proof) -> Option<Output> {
    let gamma = strict_point(pi[..32].try_into().unwrap())?;
    Some(sha512(&[&[SUITE, 0x03], gamma.mul_by_cofactor().compress().as_bytes(), &[0x00]]))
}

/// Verifies `pi` for `(key, alpha)` and returns the unique output. None: invalid key (not
/// canonical, small order, not torsion-free), gamma not canonical / torsion-free, `s` not
/// canonical, or the proof does not check.
pub fn verify(key: &[u8; 32], alpha: &[u8], pi: &Proof) -> Option<Output> {
    let y = strict_point(key)?;
    let gamma = strict_point(pi[..32].try_into().unwrap())?;
    let c: [u8; 16] = pi[32..48].try_into().unwrap();
    let s: Option<Scalar> = Scalar::from_canonical_bytes(pi[48..].try_into().unwrap()).into();
    let s = s?;
    let cs = c_scalar(&c);
    let h = encode_to_curve(key, alpha);
    let u = EdwardsPoint::vartime_double_scalar_mul_basepoint(&-cs, &y, &s);
    let v = s * h - cs * gamma;
    (challenge([&y, &h, &gamma, &u, &v]) == c).then(|| proof_to_hash(pi)).flatten()
}

/// Whether `s` (a canonical encoding) is a canonical scalar: the structural check
/// `records::nonce_fields` makes on a carried proof without the parent.
pub fn well_formed(pi: &Proof) -> bool {
    let s: Option<Scalar> = Scalar::from_canonical_bytes(pi[48..].try_into().unwrap()).into();
    s.is_some()
}

/// Number of bytes of beta the key keeps (the first byte is the class).
const KEY_BYTES: usize = 31;

/// The 32-byte final tie key: `0x00 || beta[..31]` for a registered key with a valid VRF
/// output, `0x01 || sig_hash[..31]` otherwise (the fallback, the default key SigHash).
/// Lower wins; registered tickets sort before all fallbacks.
pub fn ticket_key(vrf: Option<&Output>, registered: bool, sig_hash: &[u8; 32]) -> [u8; 32] {
    let mut k = [0u8; 32];
    match vrf {
        Some(beta) if registered => k[1..].copy_from_slice(&beta[..KEY_BYTES]),
        _ => {
            k[0] = 1;
            k[1..].copy_from_slice(&sig_hash[..KEY_BYTES]);
        }
    }
    k
}

/// One sibling's evidence: its valid VRF output (None: absent or invalid), whether its key is
/// registered for this height, and its signature hash.
#[derive(Clone, Copy, Debug)]
pub struct Candidate {
    pub vrf: Option<Output>,
    pub registered: bool,
    pub sig_hash: [u8; 32],
}

/// Whether `a` wins the tie against `b`: registered-with-valid-VRF beats everyone else; between
/// two such, the lower output; otherwise the lower signature hash. Same order as [`ticket_key`].
pub fn beats(a: &Candidate, b: &Candidate) -> bool {
    ticket_key(a.vrf.as_ref(), a.registered, &a.sig_hash) < ticket_key(b.vrf.as_ref(), b.registered, &b.sig_hash)
}

/// The VRF output a block's coinbase extra nonce proves for `parent`, if it carries a proof
/// that verifies under its miner key and the key is registered for `height`.
pub fn ticket_of_nonce(nonce: &[u8], parent: &[u8; 32], reg: &Registry, height: u64) -> Option<Output> {
    let key = sign::miner_key(nonce)?;
    let pi = crate::records::nonce_fields(nonce)?.vrf?;
    let beta = verify(&key, parent, &pi)?;
    reg.is_registered(&key, height).then_some(beta)
}

/// Default horizon: heights a key stays registered without a new accepted block or record
/// (about 70 days at 20 minutes per block).
pub const DEFAULT_HORIZON: u64 = 5000;
/// A key is registered for height `h` by a block or record at `h - REGISTER_LAG` or earlier.
pub const REGISTER_LAG: u64 = 2;

/// Keys seen signing accepted blocks or included proof records of one chain, by height.
/// Pure bookkeeping: the node feeds it (`note` from block acceptance and from record
/// verification), rolls it back on a reorg (`forget_from`) and prunes it (`prune`).
#[derive(Clone, Debug)]
pub struct Registry {
    horizon: u64,
    seen: HashMap<[u8; 32], BTreeSet<u64>>,
}

impl Default for Registry {
    fn default() -> Self {
        Registry::new(DEFAULT_HORIZON)
    }
}

impl Registry {
    pub fn new(horizon: u64) -> Registry {
        Registry { horizon, seen: HashMap::new() }
    }

    /// `key` signed an accepted block or record at `height`.
    pub fn note(&mut self, key: [u8; 32], height: u64) {
        self.seen.entry(key).or_default().insert(height);
    }

    /// Registered for height `h`: seen at `h - 2` or earlier (and not pruned).
    pub fn is_registered(&self, key: &[u8; 32], h: u64) -> bool {
        let Some(top) = h.checked_sub(REGISTER_LAG) else { return false };
        self.seen.get(key).is_some_and(|s| s.range(..=top).next().is_some())
    }

    /// Drops what is older than the horizon below `tip` (keys with nothing left are forgotten).
    pub fn prune(&mut self, tip: u64) {
        let floor = tip.saturating_sub(self.horizon);
        self.seen.retain(|_, s| {
            *s = s.split_off(&floor);
            !s.is_empty()
        });
    }

    /// Reorg: forgets everything at `height` and above.
    pub fn forget_from(&mut self, height: u64) {
        self.seen.retain(|_, s| {
            s.split_off(&height);
            !s.is_empty()
        });
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    fn arr<const N: usize>(s: &str) -> [u8; N] {
        unhex(s).try_into().unwrap()
    }

    /// RFC 9381 appendix B.3: SK seed, PK, alpha, pi, beta.
    const VECTORS: [(&str, &str, &str, &str, &str); 3] = [
        (
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            "",
            "8657106690b5526245a92b003bb079ccd1a92130477671f6fc01ad16f26f723f26f8a57ccaed74ee1b190bed1f479d9727d2d0f9b005a6e456a35d4fb0daab1268a1b0db10836d9826a528ca76567805",
            "90cf1df3b703cce59e2a35b925d411164068269d7b2d29f3301c03dd757876ff66b71dda49d2de59d03450451af026798e8f81cd2e333de5cdf4f3e140fdd8ae",
        ),
        (
            "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            "72",
            "f3141cd382dc42909d19ec5110469e4feae18300e94f304590abdced48aed5933bf0864a62558b3ed7f2fea45c92a465301b3bbf5e3e54ddf2d935be3b67926da3ef39226bbc355bdc9850112c8f4b02",
            "eb4440665d3891d668e7e0fcaf587f1b4bd7fbfe99d0eb2211ccec90496310eb5e33821bc613efb94db5e5b54c70a848a0bef4553a41befc57663b56373a5031",
        ),
        (
            "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
            "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
            "af82",
            "9bc0f79119cc5604bf02d23b4caede71393cedfbb191434dd016d30177ccbf8096bb474e53895c362d8628ee9f9ea3c0e52c7a5c691b6c18c9979866568add7a2d41b00b05081ed0f58ee5e31b3a970e",
            "645427e5d00c62a23fb703732fa5d892940935942101e456ecca7bb217c61c452118fec1219202a0edcf038bb6373241578be7217ba85a2687f7a0310b2df19f",
        ),
    ];

    /// The RFC's own scalar and nonce prefix from its seed.
    fn rfc_keys(seed: &[u8; 32]) -> (Scalar, [u8; 32], [u8; 32]) {
        let h = sha512(&[seed]);
        let mut a: [u8; 32] = h[..32].try_into().unwrap();
        a[0] &= 248;
        a[31] &= 127;
        a[31] |= 64;
        let x = Scalar::from_bytes_mod_order(a);
        let pk = (&x * curve25519_dalek::constants::ED25519_BASEPOINT_TABLE).compress().to_bytes();
        (x, pk, h[32..].try_into().unwrap())
    }

    #[test]
    fn rfc_9381_edwards25519_tai_vectors() {
        for (seed, pk, alpha, pi, beta) in VECTORS {
            let (x, my_pk, prefix) = rfc_keys(&arr(seed));
            assert_eq!(my_pk, arr::<32>(pk), "public key");
            let alpha = unhex(alpha);
            let got = prove_with(&x, &my_pk, &prefix, &alpha);
            assert_eq!(got.to_vec(), unhex(pi), "pi (prove)");
            let out = verify(&my_pk, &alpha, &got).expect("verifies");
            assert_eq!(out.to_vec(), unhex(beta), "beta");
            assert_eq!(proof_to_hash(&got).unwrap(), out);
        }
    }

    fn kp(n: u8) -> Keypair {
        Keypair::from_entropy(&[n; 32]).unwrap()
    }

    #[test]
    fn round_trip_and_failures() {
        let k = kp(7);
        let alpha = [9u8; 32];
        let pi = prove(&k, &alpha);
        let beta = verify(&k.public, &alpha, &pi).expect("round trip");
        assert_eq!(prove(&k, &alpha), pi, "deterministic");
        assert!(verify(&kp(8).public, &alpha, &pi).is_none(), "wrong key");
        assert!(verify(&k.public, &[10u8; 32], &pi).is_none(), "wrong input");
        for i in 0..PROOF_LEN {
            let mut bad = pi;
            bad[i] ^= 1;
            assert!(verify(&k.public, &alpha, &bad).is_none(), "tampered byte {i}");
        }
        // s + L is not canonical.
        let mut big = pi;
        big[48..].copy_from_slice(&[0xff; 32]);
        assert!(verify(&k.public, &alpha, &big).is_none());
        // Keys: small order, non-canonical, identity.
        let mut ident = [0u8; 32];
        ident[0] = 1;
        assert!(verify(&ident, &alpha, &pi).is_none());
        let mut noncanon = ident;
        noncanon[0] = 0xee; // y = p + 1 style encodings
        noncanon[1..31].fill(0xff);
        noncanon[31] = 0x7f;
        assert!(verify(&noncanon, &alpha, &pi).is_none());
        // A mixed-torsion key (key + a small-order point) is refused.
        let eight = CompressedEdwardsY([0x26, 0xe8, 0x95, 0x8f, 0xc2, 0xb2, 0x27, 0xb0, 0x45, 0xc3, 0xf4, 0x89, 0xf2, 0xef, 0x98, 0xf0, 0xd5, 0xdf, 0xac, 0x05, 0xd3, 0xc6, 0x33, 0x39, 0xb1, 0x38, 0x02, 0x88, 0x6d, 0x53, 0xfc, 0x05]).decompress().unwrap();
        assert!(eight.is_small_order());
        let mixed = (decode(&k.public).unwrap() + eight).compress().to_bytes();
        assert!(verify(&mixed, &alpha, &pi).is_none());
        let _ = beta;
    }

    #[test]
    fn the_output_is_unique_per_key_and_input() {
        // Many different valid proofs (different nonces) of one (key, input) carry one output;
        // the nonce prefix is the only freedom the owner has.
        let k = kp(3);
        let x = Scalar::from_canonical_bytes(k.secret).unwrap();
        let alpha = [1u8; 32];
        let first = verify(&k.public, &alpha, &prove(&k, &alpha)).unwrap();
        let mut proofs = std::collections::HashSet::new();
        for n in 0..32u8 {
            let pi = prove_with(&x, &k.public, &[n; 32], &alpha);
            assert_eq!(verify(&k.public, &alpha, &pi), Some(first), "same output for every nonce");
            proofs.insert(pi);
        }
        assert_eq!(proofs.len(), 32, "the proof bytes do differ");
        // A gamma other than x*H (here: another key's) never verifies for this key.
        let other = prove(&kp(4), &alpha);
        let mut forged = other;
        forged[32..].copy_from_slice(&prove(&k, &alpha)[32..]);
        assert!(verify(&k.public, &alpha, &forged).is_none());
        // Different inputs give different outputs (and unrelated ones).
        assert_ne!(verify(&k.public, &[2u8; 32], &prove(&k, &[2u8; 32])).unwrap(), first);
    }

    fn cand(beta: Option<u8>, registered: bool, sig: u8) -> Candidate {
        Candidate { vrf: beta.map(|b| [b; 64]), registered, sig_hash: [sig; 32] }
    }

    #[test]
    fn the_tie_rule() {
        // Registered with a valid VRF beats everyone else, whatever the fallback hash.
        assert!(beats(&cand(Some(200), true, 255), &cand(None, true, 0)));
        assert!(beats(&cand(Some(200), true, 255), &cand(Some(1), false, 0)));
        assert!(!beats(&cand(Some(1), false, 0), &cand(Some(200), true, 255)));
        // Between two such, the lower output.
        assert!(beats(&cand(Some(3), true, 9), &cand(Some(4), true, 1)));
        assert!(!beats(&cand(Some(4), true, 1), &cand(Some(3), true, 9)));
        // Otherwise the signature hash.
        assert!(beats(&cand(None, false, 1), &cand(Some(1), false, 2)));
        assert!(beats(&cand(None, true, 1), &cand(None, false, 2)));
        assert!(!beats(&cand(None, false, 2), &cand(None, false, 2)), "equal: no displacement");
    }

    #[test]
    fn registry_two_height_rule_and_pruning() {
        let (a, b) = ([1u8; 32], [2u8; 32]);
        let mut r = Registry::new(100);
        assert!(!r.is_registered(&a, 10));
        r.note(a, 10);
        assert!(!r.is_registered(&a, 10) && !r.is_registered(&a, 11), "needs h - 2");
        assert!(r.is_registered(&a, 12) && r.is_registered(&a, 50));
        assert!(!r.is_registered(&b, 50));
        assert!(!r.is_registered(&a, 1), "no underflow");
        r.note(b, 90);
        r.prune(120); // floor 20: a (10) is gone
        assert!(!r.is_registered(&a, 130) && r.is_registered(&b, 130));
        assert_eq!(r.len(), 1);
        // Reorg: forget the tail.
        r.note(b, 95);
        r.forget_from(90);
        assert!(r.is_empty());
    }

    #[test]
    fn a_carried_proof_gives_a_ticket_only_when_registered() {
        let k = kp(11);
        let parent = [5u8; 32];
        let pi = prove(&k, &parent);
        let nonce = crate::records::template_nonce_vrf(&k.public, None, None, Some(&pi));
        let mut reg = Registry::default();
        assert!(ticket_of_nonce(&nonce, &parent, &reg, 100).is_none(), "unregistered");
        reg.note(k.public, 98);
        let t = ticket_of_nonce(&nonce, &parent, &reg, 100).expect("registered 2 heights earlier");
        assert_eq!(Some(t), verify(&k.public, &parent, &pi));
        assert!(ticket_of_nonce(&nonce, &parent, &reg, 99).is_none(), "one height is too late");
        assert!(ticket_of_nonce(&nonce, &[6u8; 32], &reg, 100).is_none(), "wrong parent");
    }
}
