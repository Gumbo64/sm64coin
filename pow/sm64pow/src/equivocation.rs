//! Equivocation: one miner key, two runs for the same template (layer 5,
//! relay policy in the style of admit.rs; nothing here changes validity).
//!
//! With signed proofs (sign.rs) only the miner can make another valid run
//! for its template, and the template commits the key, so "same key" and
//! "same seed" are the same thing. What a miner may do with that:
//!
//! - **Before its block is valid** (held: hold.rs, `Blockchain::sm64_hold`)
//!   it may submit any number of strictly shorter runs for the template;
//!   each replaces the held one ([`held_replaces`]). That is the auction
//!   working: never misbehaviour.
//! - **After a block for the template is accepted** (it is in our chain),
//!   another run for the same template arrives as an alternative block
//!   at the same height ([`judge_sibling`]):
//!   - the same run (same proof_id: a copy, a restamp): ignored;
//!   - strictly shorter: ignored, no penalty, no tip takeover (otherwise a
//!     miner could drip-feed late overrides of its own block);
//!   - the same length, another run: **equivocation**. An honest miner only
//!     ever signs strictly shorter runs, so two signed runs of one length
//!     are proof of a deliberate split (e.g. A to half the network, A' to the
//!     other half). The node keeps compact [`Evidence`] (both signed runs),
//!     relays it (`NOTIFY_SM64_EVIDENCE`: [`Policy::take_outbox`], docs/V0.3-NEW-CHAIN.md) and
//!     refuses to build on that template's block while it is childless
//!     ([`Policy::avoid`], [`choose_parent`]): miners build a sibling, and
//!     the equivocator's block loses the race;
//!   - strictly longer: ignored. See [`LONGER_IS_EVIDENCE`] for why it is
//!     not evidence by default.
//!
//! Evidence verifies without the blocks: both signatures sign
//! `(seed, proof_id, frames)` ([`crate::sign::message`]). Whether the key is
//! the template's own is checked against the coinbase of our block with that
//! seed ([`Policy::note_key`]), so evidence about a seed we never saw waits.
//!
//! Read next: chain.rs.

use crate::admit::Bounded;
use crate::sign;
use std::sync::{Mutex, OnceLock};

/// Whether a strictly longer second run (after the first was accepted) is
/// equivocation evidence too. The design list says "equal-or-longer"; it is
/// off because an honest miner produces exactly that pattern: it submits run
/// A, then a shorter A' before either is valid; A' becomes valid first and
/// propagates, but a node that held A and had not seen A' when A became due
/// relays A, and a third node then sees A (longer) after A' (accepted).
/// Equal lengths never happen honestly. Turn it on only with a margin rule.
pub const LONGER_IS_EVIDENCE: bool = false;

const SEEDS: usize = 65536;

/// One signed run: what a signature commits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignedRun {
    pub proof_id: [u8; 32],
    pub frames: u32,
    pub sig: [u8; 64],
}

/// Two signed runs for one template under one key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub seed: [u8; 32],
    pub key: [u8; 32],
    pub a: SignedRun,
    pub b: SignedRun,
}

const EVIDENCE_TAG: &[u8; 8] = b"sm64ev1\0";
/// Encoded size: tag, seed, key, two runs of proof_id + frames + signature.
pub const EVIDENCE_LEN: usize = 8 + 32 + 32 + 2 * (32 + 4 + 64);

impl Evidence {
    /// Whether this is evidence at all: two different runs, lengths that an
    /// honest miner never signs in this order, both signed by `key`.
    pub fn verify(&self) -> bool {
        if self.a.proof_id == self.b.proof_id {
            return false;
        }
        let lengths = self.a.frames == self.b.frames || LONGER_IS_EVIDENCE;
        lengths
            && sign::verify(&self.key, &sign::message(&self.seed, &self.a.proof_id, self.a.frames), &self.a.sig)
            && sign::verify(&self.key, &sign::message(&self.seed, &self.b.proof_id, self.b.frames), &self.b.sig)
    }

    /// The order of the two runs doesn't matter: `a` is the smaller proof_id.
    fn canonical(mut self) -> Self {
        if self.b.proof_id < self.a.proof_id {
            std::mem::swap(&mut self.a, &mut self.b);
        }
        self
    }

    pub fn encode(&self) -> Vec<u8> {
        let e = self.canonical();
        let mut v = Vec::with_capacity(EVIDENCE_LEN);
        v.extend_from_slice(EVIDENCE_TAG);
        v.extend_from_slice(&e.seed);
        v.extend_from_slice(&e.key);
        for r in [&e.a, &e.b] {
            v.extend_from_slice(&r.proof_id);
            v.extend_from_slice(&r.frames.to_le_bytes());
            v.extend_from_slice(&r.sig);
        }
        v
    }

    pub fn decode(b: &[u8]) -> Option<Evidence> {
        if b.len() != EVIDENCE_LEN || &b[..8] != EVIDENCE_TAG {
            return None;
        }
        let arr = |o: usize| -> [u8; 32] { b[o..o + 32].try_into().unwrap() };
        let run = |o: usize| SignedRun {
            proof_id: arr(o),
            frames: u32::from_le_bytes(b[o + 32..o + 36].try_into().unwrap()),
            sig: b[o + 36..o + 100].try_into().unwrap(),
        };
        Some(Evidence { seed: arr(8), key: arr(40), a: run(72), b: run(172) })
    }
}

/// Whether a new run for the template of a held (not yet valid) block
/// replaces it: strictly shorter only.
pub fn held_replaces(held_frames: u32, new_frames: u32) -> bool {
    new_frames < held_frames
}

/// What to do with a second run for the template of an accepted block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sibling {
    /// The same run (a copy or a restamp).
    SameRun,
    /// Strictly shorter, arriving after the block was accepted: ignored.
    LateShorter,
    /// Strictly longer: ignored.
    Longer,
    /// The signature doesn't verify: an invalid block.
    BadSignature,
    /// Two signed runs of the same length (or longer, see [`LONGER_IS_EVIDENCE`]).
    Equivocation(Evidence),
}

/// Judges `new` against `accepted` (our block for the same template, `seed`,
/// whose coinbase commits `key`).
pub fn judge_sibling(seed: &[u8; 32], key: &[u8; 32], accepted: &SignedRun, new: &SignedRun) -> Sibling {
    if new.proof_id == accepted.proof_id && new.frames == accepted.frames {
        return Sibling::SameRun;
    }
    if !sign::verify(key, &sign::message(seed, &new.proof_id, new.frames), &new.sig) {
        return Sibling::BadSignature;
    }
    if new.frames < accepted.frames {
        return Sibling::LateShorter;
    }
    if new.frames > accepted.frames && !LONGER_IS_EVIDENCE {
        return Sibling::Longer;
    }
    Sibling::Equivocation(Evidence { seed: *seed, key: *key, a: *accepted, b: *new }.canonical())
}

/// What the node knows: template keys and evidence. One per process ([`policy`]).
pub struct Policy {
    /// The miner key of every template whose signature we checked.
    keys: Bounded<[u8; 32], [u8; 32]>,
    evidence: Bounded<[u8; 32], Evidence>,
    /// New evidence to relay to peers (the daemon drains it: [`Policy::take_outbox`]).
    outbox: Vec<Evidence>,
    /// Evidence seen first (to relay once): counted for logs and tests.
    pub received: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Policy { keys: Bounded::new(SEEDS), evidence: Bounded::new(SEEDS), outbox: Vec::new(), received: 0 }
    }
}

impl Policy {
    /// A signature checked: template `seed` commits miner key `key`.
    pub fn note_key(&mut self, seed: &[u8; 32], key: &[u8; 32]) {
        self.keys.insert(*seed, *key);
    }

    /// Takes evidence (found here or received from a peer). True when it is
    /// new and valid: relay it.
    pub fn add(&mut self, e: Evidence) -> bool {
        let e = e.canonical();
        if self.evidence.get(&e.seed).is_some() || !e.verify() {
            return false;
        }
        self.evidence.insert(e.seed, e);
        self.received += 1;
        if self.outbox.len() < 64 {
            self.outbox.push(e);
        }
        true
    }

    /// The next new evidence to relay, if any.
    pub fn take_outbox(&mut self) -> Option<Evidence> {
        self.outbox.pop()
    }

    /// Whether template `seed`'s block is equivocated: valid evidence whose
    /// key is the one that template's coinbase commits.
    pub fn avoid(&self, seed: &[u8; 32]) -> bool {
        match (self.evidence.get(seed), self.keys.get(seed)) {
            (Some(e), Some(k)) => e.key == *k,
            _ => false,
        }
    }

    pub fn evidence(&self, seed: &[u8; 32]) -> Option<Evidence> {
        self.evidence.get(seed).copied()
    }
}

pub fn policy() -> &'static Mutex<Policy> {
    static P: OnceLock<Mutex<Policy>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(Policy::default()))
}

/// Where a template should build when the tip is a childless block of
/// template `tip_seed`: `None` on the tip; `Some(i)` on `siblings[i]` (blocks
/// at the tip's height, by seed) when the tip is equivocated and a clean
/// sibling exists; `Some(usize::MAX)` on the tip's parent (build a sibling).
pub fn choose_parent(p: &Policy, tip_seed: &[u8; 32], siblings: &[[u8; 32]]) -> Option<usize> {
    if !p.avoid(tip_seed) {
        return None;
    }
    Some(siblings.iter().position(|s| s != tip_seed && !p.avoid(s)).unwrap_or(usize::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sign::Keypair;

    fn run(k: &Keypair, seed: &[u8; 32], pid: u8, frames: u32) -> SignedRun {
        let proof_id = [pid; 32];
        SignedRun { proof_id, frames, sig: k.sign_run(seed, &proof_id, frames) }
    }

    #[test]
    fn shorter_runs_supersede_only_while_held() {
        assert!(held_replaces(1000, 999));
        assert!(!held_replaces(1000, 1000));
        assert!(!held_replaces(1000, 1001));
        let k = Keypair::from_entropy(&[1; 32]).unwrap();
        let seed = [9; 32];
        let a = run(&k, &seed, 1, 1000);
        assert_eq!(judge_sibling(&seed, &k.public, &a, &run(&k, &seed, 2, 900)), Sibling::LateShorter, "after acceptance: ignored, never misbehaviour");
        assert_eq!(judge_sibling(&seed, &k.public, &a, &a), Sibling::SameRun);
        assert_eq!(judge_sibling(&seed, &k.public, &a, &run(&k, &seed, 3, 1100)), Sibling::Longer);
    }

    #[test]
    fn equal_length_second_run_is_evidence_that_verifies_and_roundtrips() {
        let k = Keypair::from_entropy(&[2; 32]).unwrap();
        let seed = [8; 32];
        let a = run(&k, &seed, 1, 1000);
        let b = run(&k, &seed, 2, 1000);
        let Sibling::Equivocation(e) = judge_sibling(&seed, &k.public, &a, &b) else { panic!() };
        assert!(e.verify());
        let bytes = e.encode();
        assert_eq!(bytes.len(), EVIDENCE_LEN);
        assert_eq!(Evidence::decode(&bytes), Some(e));
        // Same evidence whichever run was first.
        let Sibling::Equivocation(e2) = judge_sibling(&seed, &k.public, &b, &a) else { panic!() };
        assert_eq!(e2.encode(), bytes);
    }

    #[test]
    fn forged_evidence_is_refused() {
        let (k, attacker) = (Keypair::from_entropy(&[3; 32]).unwrap(), Keypair::from_entropy(&[4; 32]).unwrap());
        let seed = [7; 32];
        let a = run(&k, &seed, 1, 1000);
        // A second run "signed" with another key.
        let b = run(&attacker, &seed, 2, 1000);
        assert_eq!(judge_sibling(&seed, &k.public, &a, &b), Sibling::BadSignature);
        let fake = Evidence { seed, key: k.public, a, b };
        assert!(!fake.verify());
        // An attacker's own key over our seed verifies but doesn't match the coinbase.
        let mut p = Policy::default();
        p.note_key(&seed, &k.public);
        let own = Evidence { seed, key: attacker.public, a: run(&attacker, &seed, 1, 5), b: run(&attacker, &seed, 2, 5) };
        assert!(p.add(own));
        assert!(!p.avoid(&seed), "the key isn't the template's");
        // The same run twice, or bytes that don't decode.
        assert!(!Evidence { seed, key: k.public, a, b: a }.verify());
        assert!(Evidence::decode(&[0; 10]).is_none());
    }

    #[test]
    fn equivocated_tip_is_not_built_on_while_a_clean_sibling_or_the_parent_exists() {
        let k = Keypair::from_entropy(&[5; 32]).unwrap();
        let (seed, clean) = ([1; 32], [2; 32]);
        let mut p = Policy::default();
        assert_eq!(choose_parent(&p, &seed, &[]), None);
        p.note_key(&seed, &k.public);
        let Sibling::Equivocation(e) = judge_sibling(&seed, &k.public, &run(&k, &seed, 1, 700), &run(&k, &seed, 2, 700)) else { panic!() };
        assert!(p.add(e));
        assert!(!p.add(e), "relayed once");
        assert_eq!(p.take_outbox(), Some(e.canonical()));
        assert_eq!(p.take_outbox(), None);
        assert!(p.avoid(&seed));
        assert_eq!(choose_parent(&p, &seed, &[seed]), Some(usize::MAX), "no clean sibling: build on the parent");
        assert_eq!(choose_parent(&p, &seed, &[seed, clean]), Some(1));
        assert_eq!(choose_parent(&p, &clean, &[seed]), None);
    }
}
