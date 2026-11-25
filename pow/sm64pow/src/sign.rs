//! Signed proofs: the miner key and the block signature (layer 1, pure).
//!
//! Every block's run can be changed a little and still be valid (the last
//! inputs differ, the star lands on the same frame: docs/DOS.md), and the
//! seed does not cover the proof, so before v0.3 anyone could mint *valid*
//! variants of a published block for free. From v0.3 (a new chain) a block
//! binds its run to its miner:
//!
//! - The miner generates a throwaway key pair per template and puts the
//!   public key in the coinbase's extra nonce, as [`KEY_TAG`] `||` key
//!   (`get_block_template` with `extra_nonce`; more bytes may follow, for
//!   pools). The coinbase is in the block hashing blob, so the key is inside
//!   the seed: one template, one key.
//! - The block carries `sm64_sig`, a signature with that key over
//!   [`message`]`(seed, proof_id, frames)`. Nodes check it (about 0.1 ms)
//!   before any replay. Without the secret key nobody can sign another run
//!   for that template, so third-party variants are invalid at the cost of
//!   one signature check.
//!
//! The scheme is Monero's own (`crypto::generate_signature` /
//! `crypto::check_signature`, a Schnorr signature on ed25519 with Keccak):
//! `(c, r)` with `c = H(m || P || rG + cP)`, both scalars canonical and `c`
//! non-zero. It is byte-compatible with the daemon's C++, so stock Monero
//! code can check these signatures too, and pure Rust here (curve25519-dalek,
//! already used by the leaderboard), so the browser miner signs in WASM.
//! Signing is deterministic (the nonce is derived from the secret key and the
//! message): signing the same run twice gives the same signature, hence the
//! same block id. Third parties cannot change a signature: both scalars must
//! be canonical (no `r + L`), and producing another valid `(c, r)` for the
//! same message is a forgery.
//!
//! What is signed: the seed (the template, timestamp excluded, so restamping
//! a block keeps its signature), the run's `proof_id` (what the block id
//! commits to; unchanged when the daemon re-encodes a stateless proof), and
//! the claimed frames, so the length of a signed run is known without
//! decoding it (equivocation evidence compares lengths: equivocation.rs).
//!
//! Read next: rules.rs.

use curve25519_dalek::constants::ED25519_BASEPOINT_TABLE;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use sha3::{Digest, Keccak256, Keccak512};

/// The first byte of the coinbase extra nonce that carries the miner key.
pub const KEY_TAG: u8 = 0x64;
/// The extra nonce a miner asks for: `KEY_TAG || public key` (33 bytes).
pub const KEY_NONCE_LEN: usize = 33;
/// Domain separation for [`message`].
const MSG_TAG: &[u8] = b"sm64coin proof signature v1";
const NONCE_TAG: &[u8] = b"sm64coin signing nonce v1";

/// `cn_fast_hash`: Keccak-256 with the original padding.
pub fn cn_fast_hash(data: &[u8]) -> [u8; 32] {
    Keccak256::digest(data).into()
}

/// Monero's `hash_to_scalar`: Keccak-256, reduced mod l.
fn hash_to_scalar(data: &[u8]) -> Scalar {
    Scalar::from_bytes_mod_order(cn_fast_hash(data))
}

/// What a block's signature signs: the seed, the run's proof_id and its
/// claimed length in frames.
pub fn message(seed: &[u8; 32], proof_id: &[u8; 32], frames: u32) -> [u8; 32] {
    let mut b = Vec::with_capacity(MSG_TAG.len() + 68);
    b.extend_from_slice(MSG_TAG);
    b.extend_from_slice(seed);
    b.extend_from_slice(proof_id);
    b.extend_from_slice(&frames.to_le_bytes());
    cn_fast_hash(&b)
}

/// A miner key pair (a Monero-style secret scalar and its public point).
#[derive(Clone)]
pub struct Keypair {
    pub secret: [u8; 32],
    pub public: [u8; 32],
}

impl std::fmt::Debug for Keypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Keypair {{ public: {} }}", hex(&self.public))
    }
}

impl Keypair {
    /// From 32 bytes of entropy (reduced mod l, like Monero's `generate_keys`
    /// with a recovery key). Zero entropy is refused.
    pub fn from_entropy(entropy: &[u8; 32]) -> Option<Keypair> {
        let s = Scalar::from_bytes_mod_order(*entropy);
        Keypair::from_secret(&s.to_bytes())
    }

    /// From a canonical secret scalar (the key a group leader shares).
    pub fn from_secret(secret: &[u8; 32]) -> Option<Keypair> {
        let s: Option<Scalar> = Scalar::from_canonical_bytes(*secret).into();
        let s = s?;
        if s == Scalar::ZERO {
            return None;
        }
        let public = (&s * ED25519_BASEPOINT_TABLE).compress().to_bytes();
        Some(Keypair { secret: s.to_bytes(), public })
    }

    /// A fresh random key pair (the operating system's randomness).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn generate() -> Keypair {
        loop {
            let mut e = [0u8; 32];
            getrandom::getrandom(&mut e).expect("the operating system's randomness");
            if let Some(k) = Keypair::from_entropy(&e) {
                return k;
            }
        }
    }

    /// The coinbase extra nonce that commits this key (`KEY_TAG || public`).
    pub fn extra_nonce(&self) -> [u8; KEY_NONCE_LEN] {
        key_nonce(&self.public)
    }

    /// Signs `msg` (deterministic; the same message gives the same signature).
    pub fn sign(&self, msg: &[u8; 32]) -> [u8; 64] {
        let x = Scalar::from_canonical_bytes(self.secret).unwrap();
        let mut n = Vec::with_capacity(NONCE_TAG.len() + 64);
        n.extend_from_slice(NONCE_TAG);
        n.extend_from_slice(&self.secret);
        n.extend_from_slice(msg);
        let mut ctr = 0u8;
        loop {
            let mut h = n.clone();
            h.push(ctr);
            let wide: [u8; 64] = Keccak512::digest(&h).into();
            let k = Scalar::from_bytes_mod_order_wide(&wide);
            let comm = (&k * ED25519_BASEPOINT_TABLE).compress().to_bytes();
            let mut buf = [0u8; 96];
            buf[..32].copy_from_slice(msg);
            buf[32..64].copy_from_slice(&self.public);
            buf[64..].copy_from_slice(&comm);
            let c = hash_to_scalar(&buf);
            let r = k - c * x;
            if c != Scalar::ZERO && r != Scalar::ZERO && k != Scalar::ZERO {
                let mut sig = [0u8; 64];
                sig[..32].copy_from_slice(c.as_bytes());
                sig[32..].copy_from_slice(r.as_bytes());
                return sig;
            }
            ctr = ctr.wrapping_add(1);
        }
    }

    /// Signs a block's run: [`message`]`(seed, proof_id, frames)`.
    pub fn sign_run(&self, seed: &[u8; 32], proof_id: &[u8; 32], frames: u32) -> [u8; 64] {
        self.sign(&message(seed, proof_id, frames))
    }
}

/// The extra nonce that commits public key `key`.
pub fn key_nonce(key: &[u8; 32]) -> [u8; KEY_NONCE_LEN] {
    let mut n = [0u8; KEY_NONCE_LEN];
    n[0] = KEY_TAG;
    n[1..].copy_from_slice(key);
    n
}

/// The miner key a coinbase extra nonce commits (None: no key, or a key that
/// is not a valid, canonical, torsion-free curve point).
pub fn miner_key(extra_nonce: &[u8]) -> Option<[u8; 32]> {
    if extra_nonce.len() < KEY_NONCE_LEN || extra_nonce[0] != KEY_TAG {
        return None;
    }
    let k: [u8; 32] = extra_nonce[1..KEY_NONCE_LEN].try_into().unwrap();
    point(&k).map(|_| k)
}

/// A public key as a curve point: canonical encoding, not of small order
/// and torsion-free (stricter than Monero's `check_key`; the miner picks its
/// key, so honest keys always pass).
fn point(k: &[u8; 32]) -> Option<EdwardsPoint> {
    let p = CompressedEdwardsY(*k).decompress()?;
    if p.compress().to_bytes() != *k || p.is_small_order() || !p.is_torsion_free() {
        return None;
    }
    Some(p)
}

/// Monero's `check_signature` over the 32-byte `msg`.
pub fn verify(key: &[u8; 32], msg: &[u8; 32], sig: &[u8; 64]) -> bool {
    let Some(p) = point(key) else { return false };
    let c: Option<Scalar> = Scalar::from_canonical_bytes(sig[..32].try_into().unwrap()).into();
    let r: Option<Scalar> = Scalar::from_canonical_bytes(sig[32..].try_into().unwrap()).into();
    let (Some(c), Some(r)) = (c, r) else { return false };
    if c == Scalar::ZERO {
        return false;
    }
    let comm = EdwardsPoint::vartime_double_scalar_mul_basepoint(&c, &p, &r).compress();
    if comm == EdwardsPoint::default().compress() {
        return false; // the identity
    }
    let mut buf = [0u8; 96];
    buf[..32].copy_from_slice(msg);
    buf[32..64].copy_from_slice(key);
    buf[64..].copy_from_slice(comm.as_bytes());
    hash_to_scalar(&buf) == c
}

/// Checks a block's signature: the coinbase extra nonce must commit a miner
/// key, and `sig` must sign [`message`]`(seed, proof_id, frames)` with it.
pub fn check(seed: &[u8; 32], extra_nonce: &[u8], proof_id: &[u8; 32], frames: u32, sig: &[u8; 64]) -> Result<[u8; 32], &'static str> {
    let key = miner_key(extra_nonce).ok_or("the coinbase commits no miner key (extra nonce 0x64 || key)")?;
    if !verify(&key, &message(seed, proof_id, frames), sig) {
        return Err("the proof's signature does not verify with the coinbase's miner key");
    }
    Ok(key)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kp(b: u8) -> Keypair {
        Keypair::from_entropy(&[b; 32]).unwrap()
    }

    #[test]
    fn sign_verify_roundtrip_and_deterministic() {
        let k = kp(7);
        let m = message(&[1; 32], &[2; 32], 1234);
        let s = k.sign(&m);
        assert!(verify(&k.public, &m, &s));
        assert_eq!(s, k.sign(&m), "deterministic");
        assert_ne!(s, k.sign(&message(&[1; 32], &[2; 32], 1235)));
    }

    #[test]
    fn wrong_key_message_or_bits_fail() {
        let (k, other) = (kp(7), kp(8));
        let m = message(&[1; 32], &[2; 32], 1234);
        let s = k.sign(&m);
        assert!(!verify(&other.public, &m, &s), "wrong key");
        assert!(!verify(&k.public, &message(&[1; 32], &[3; 32], 1234), &s), "another proof");
        assert!(!verify(&k.public, &message(&[1; 32], &[2; 32], 1233), &s), "another length");
        assert!(!verify(&k.public, &message(&[9; 32], &[2; 32], 1234), &s), "another template");
        for i in [0, 31, 32, 63] {
            let mut t = s;
            t[i] ^= 1;
            assert!(!verify(&k.public, &m, &t), "bit {i}");
        }
        assert!(!verify(&k.public, &m, &[0; 64]));
    }

    #[test]
    fn non_canonical_scalars_are_rejected() {
        // r + l encodes the same scalar mod l: a third party's free variant
        // of the signature if it were accepted.
        let k = kp(5);
        let m = message(&[4; 32], &[5; 32], 99);
        let s = k.sign(&m);
        let l: [u8; 32] = [
            0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0x10,
        ];
        for half in [0usize, 32] {
            let mut t = s;
            let mut carry = 0u16;
            for i in 0..32 {
                let v = t[half + i] as u16 + l[i] as u16 + carry;
                t[half + i] = v as u8;
                carry = v >> 8;
            }
            if carry == 0 {
                assert!(!verify(&k.public, &m, &t), "scalar + l at {half}");
            }
        }
    }

    #[test]
    fn miner_key_from_the_extra_nonce() {
        let k = kp(3);
        let n = k.extra_nonce();
        assert_eq!(miner_key(&n), Some(k.public));
        let mut longer = n.to_vec();
        longer.extend_from_slice(b"pool worker 17");
        assert_eq!(miner_key(&longer), Some(k.public), "pools may append bytes");
        assert_eq!(miner_key(&n[..32]), None, "too short");
        let mut wrong = n;
        wrong[0] = 0;
        assert_eq!(miner_key(&wrong), None, "a payment id, not a key");
        assert_eq!(miner_key(&[]), None);
        // The identity and small-order points are not keys.
        let mut id = [0u8; 32];
        id[0] = 1;
        assert_eq!(miner_key(&key_nonce(&id)), None);
    }

    #[test]
    fn check_ties_the_signature_to_the_coinbase_key() {
        let (k, other) = (kp(1), kp(2));
        let (seed, pid) = ([6; 32], [7; 32]);
        let sig = k.sign_run(&seed, &pid, 500);
        assert_eq!(check(&seed, &k.extra_nonce(), &pid, 500, &sig), Ok(k.public));
        assert!(check(&seed, &other.extra_nonce(), &pid, 500, &sig).is_err(), "key not in the coinbase");
        assert!(check(&seed, &[], &pid, 500, &sig).is_err(), "no key");
        assert!(check(&seed, &k.extra_nonce(), &[8; 32], 500, &sig).is_err(), "signature over another proof");
    }

    #[test]
    fn secret_roundtrip() {
        let k = kp(11);
        let again = Keypair::from_secret(&k.secret).unwrap();
        assert_eq!(again.public, k.public);
        assert!(Keypair::from_secret(&[0; 32]).is_none());
        assert!(Keypair::from_secret(&[0xff; 32]).is_none(), "not canonical");
    }
}
