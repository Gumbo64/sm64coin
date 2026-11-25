//! Equivocation: one miner key, one run per template (layer 5, relay policy
//! in the style of admit.rs; nothing here changes validity).
//!
//! With signed proofs (sign.rs) only the miner can make another valid run
//! for its template, and the template commits the key, so "same key" and
//! "same seed" are the same thing. **One run per key**: a miner signs at
//! most one run for its template. Any two signed runs of one seed that
//! differ (another `proof_id`, or another length) are equivocation, whatever
//! their lengths and whether or not either block is valid yet. There is no
//! length comparison and no "a shorter run replaces the held one": a miner
//! that wants to keep improving keeps its improvements private and
//! publishes its best run once, just before it is due (docs/V0.3-NEW-CHAIN.md,
//! "Why one run per key"). A re-sent copy of the same run (same `proof_id`
//! and length; signatures are deterministic, so also the same block, or the
//! same run restamped) is just a duplicate.
//!
//! What a node does ([`Policy::claim`], called for every block whose
//! signature checked, on arrival and again when it is verified, and
//! [`judge_sibling`] for a block already in our chain):
//!
//! - the first signed run it sees for a seed is the **claim**: it goes
//!   through the normal path (held until due, replayed, accepted);
//! - a second, different signed run for the seed is **not replayed and not
//!   relayed** as a block; the node keeps compact [`Evidence`] (272 bytes: tag,
//!   seed, key, two `(proof_id, frames, sig)`, verifiable without the
//!   blocks), logs it, relays it (`NOTIFY_SM64_EVIDENCE`:
//!   [`Policy::take_outbox`]);
//! - the **first run stays as it was**: held as normal, accepted when due. But
//!   once a template has evidence, the node refuses to build on that
//!   template's block while it is childless ([`Policy::avoid`],
//!   [`choose_parent`]): miners build a sibling, and the equivocator's block
//!   loses the race. Nothing is penalised on the relaying peer (an honest
//!   node may relay either run).
//!
//! Evidence verifies without the blocks: both signatures sign
//! `(seed, proof_id, frames)` ([`crate::sign::message`]). Whether the key is
//! the template's own is checked against the coinbase of the block with that
//! seed ([`Policy::note_key`]), so evidence about a seed we never saw waits.
//!
//! Read next: chain.rs.

use crate::admit::Bounded;
use crate::sign;
use std::sync::{Mutex, OnceLock};

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
    /// Whether this is evidence at all: two different runs (another
    /// `proof_id` or another length), both signed by `key` for `seed`.
    pub fn verify(&self) -> bool {
        if self.a.proof_id == self.b.proof_id && self.a.frames == self.b.frames {
            return false;
        }
        sign::verify(&self.key, &sign::message(&self.seed, &self.a.proof_id, self.a.frames), &self.a.sig)
            && sign::verify(&self.key, &sign::message(&self.seed, &self.b.proof_id, self.b.frames), &self.b.sig)
    }

    /// The order of the two runs doesn't matter: `a` is the smaller proof_id.
    fn canonical(mut self) -> Self {
        if (self.b.proof_id, self.b.frames) < (self.a.proof_id, self.a.frames) {
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

/// What to do with another run for the template of a block in our chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sibling {
    /// The same run (a copy or a restamp): a duplicate.
    SameRun,
    /// The signature doesn't verify: an invalid block.
    BadSignature,
    /// Two different signed runs of one template: one run per key.
    Equivocation(Evidence),
}

/// Judges `new` against `accepted` (our block for the same template, `seed`,
/// whose coinbase commits `key`): the same run is a duplicate, any other
/// signed run is equivocation, whatever its length.
pub fn judge_sibling(seed: &[u8; 32], key: &[u8; 32], accepted: &SignedRun, new: &SignedRun) -> Sibling {
    if new.proof_id == accepted.proof_id && new.frames == accepted.frames {
        return Sibling::SameRun;
    }
    if !sign::verify(key, &sign::message(seed, &new.proof_id, new.frames), &new.sig) {
        return Sibling::BadSignature;
    }
    Sibling::Equivocation(Evidence { seed: *seed, key: *key, a: *accepted, b: *new }.canonical())
}

/// What [`Policy::claim`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// The first signed run seen for this template (now its claim).
    First,
    /// The same run again.
    Same,
    /// A different run than the claim: evidence (already kept, queued for relay when new).
    Equivocation(Evidence),
}

/// What the node knows: template keys and evidence. One per process ([`policy`]).
pub struct Policy {
    /// The first signed run seen per template.
    claims: Bounded<[u8; 32], SignedRun>,
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
        Policy { claims: Bounded::new(SEEDS), keys: Bounded::new(SEEDS), evidence: Bounded::new(SEEDS), outbox: Vec::new(), received: 0 }
    }
}

impl Policy {
    /// A signature checked: template `seed` commits miner key `key`.
    pub fn note_key(&mut self, seed: &[u8; 32], key: &[u8; 32]) {
        self.keys.insert(*seed, *key);
    }

    /// A run whose signature checked (`key` is its coinbase's miner key):
    /// the first for its seed is the claim; any different one is
    /// equivocation, and its evidence is kept and queued for relay.
    pub fn claim(&mut self, seed: &[u8; 32], key: &[u8; 32], run: &SignedRun) -> Claim {
        match self.claims.get(seed).copied() {
            None => {
                self.claims.insert(*seed, *run);
                Claim::First
            }
            Some(first) if first.proof_id == run.proof_id && first.frames == run.frames => Claim::Same,
            Some(first) => {
                let e = Evidence { seed: *seed, key: *key, a: first, b: *run }.canonical();
                self.add(e);
                Claim::Equivocation(e)
            }
        }
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
    fn any_second_different_run_is_equivocation_whatever_its_length() {
        let k = Keypair::from_entropy(&[1; 32]).unwrap();
        let seed = [9; 32];
        let a = run(&k, &seed, 1, 1000);
        assert_eq!(judge_sibling(&seed, &k.public, &a, &a), Sibling::SameRun);
        for (pid, frames) in [(2, 900), (3, 1000), (4, 1100)] {
            assert!(matches!(judge_sibling(&seed, &k.public, &a, &run(&k, &seed, pid, frames)), Sibling::Equivocation(e) if e.verify()), "shorter, equal, longer: all evidence ({frames})");
        }
        // The same proof at another length (the length is signed too) is another run.
        let b = run(&k, &seed, 1, 999);
        assert!(matches!(judge_sibling(&seed, &k.public, &a, &b), Sibling::Equivocation(e) if e.verify()));
    }

    #[test]
    fn the_first_claim_stands_and_a_different_second_run_is_evidence_once() {
        let k = Keypair::from_entropy(&[6; 32]).unwrap();
        let (seed, other) = ([5; 32], [6; 32]);
        let mut p = Policy::default();
        p.note_key(&seed, &k.public);
        let first = run(&k, &seed, 1, 1000);
        assert_eq!(p.claim(&seed, &k.public, &first), Claim::First);
        assert_eq!(p.claim(&seed, &k.public, &first), Claim::Same, "a re-sent copy is a duplicate");
        assert!(!p.avoid(&seed) && p.take_outbox().is_none());
        // Another template of the same key is its own claim (it is another seed).
        assert_eq!(p.claim(&other, &k.public, &run(&k, &other, 1, 10)), Claim::First);
        // A shorter run: not a replacement, evidence.
        let shorter = run(&k, &seed, 2, 600);
        let Claim::Equivocation(e) = p.claim(&seed, &k.public, &shorter) else { panic!("shorter is evidence too") };
        assert!(e.verify() && p.avoid(&seed));
        assert_eq!(p.take_outbox(), Some(e));
        // A third run, or the second again: the evidence is kept, relayed once.
        assert!(matches!(p.claim(&seed, &k.public, &run(&k, &seed, 3, 5000)), Claim::Equivocation(_)));
        assert!(matches!(p.claim(&seed, &k.public, &shorter), Claim::Equivocation(_)));
        assert_eq!(p.take_outbox(), None);
        assert_eq!(p.received, 1);
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
