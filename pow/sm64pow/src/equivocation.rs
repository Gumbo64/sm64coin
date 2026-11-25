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
use crate::template::{self, TemplateParts};
use std::cell::RefCell;
use std::sync::{Mutex, OnceLock};

/// Evidence on the wire (`NOTIFY_SM64_EVIDENCE`) is self-contained: the 272
/// bytes of [`Evidence::encode`] followed by the accused template
/// ([`template::encode`]: header, coinbase, tx hashes). A node checks the
/// two signatures and that the template's seed is the evidence's seed and
/// its coinbase commits the evidence's key, without having the block. So
/// nobody can make evidence about a template that isn't theirs.
pub const MAX_WIRE: usize = EVIDENCE_LEN + 16 + template::MAX_HEADER + template::MAX_MINER_TX + 32 * template::MAX_TX_HASHES;
/// Evidence counts for fork choice ([`Policy::fork_choice`]) only if this node
/// had it while the accused block was at most this deep (0: the tip), so
/// evidence that surfaces late can't be used for a deep reorg.
pub const FORK_CHOICE_DEPTH: u64 = 2;
/// Recent evidence (by the accused block's height) sent to a newly connected
/// peer and kept across restarts.
pub const RECENT_HEIGHTS: u64 = 8;
const WIRES: usize = 1024;

thread_local! {
    /// The block being handled on this thread (the daemon's add_new_block), as a template, its seed and our top height.
    static BLOCK: RefCell<Option<(TemplateParts, [u8; 32], u64)>> = const { RefCell::new(None) };
}

/// While alive, evidence formed on this thread about `blob`'s template carries it (so it can be relayed).
pub struct BlockScope(Option<(TemplateParts, [u8; 32], u64)>);
impl Drop for BlockScope {
    fn drop(&mut self) {
        let prev = self.0.take();
        BLOCK.with(|b| *b.borrow_mut() = prev);
    }
}
/// `blob`: a full block; `top`: the height of our chain's top block.
pub fn block_scope(blob: &[u8], top: u64) -> BlockScope {
    let cur = template::split_block(blob).ok().and_then(|p| template::facts(&p).ok().map(|f| (p, f.seed, top)));
    BlockScope(BLOCK.with(|b| std::mem::replace(&mut *b.borrow_mut(), cur)))
}
fn scoped(seed: &[u8; 32]) -> Option<(TemplateParts, u64)> {
    BLOCK.with(|b| b.borrow().as_ref().filter(|(_, s, _)| s == seed).map(|(p, _, t)| (p.clone(), *t)))
}

/// What we keep per accused template, beyond the [`Evidence`].
#[derive(Clone)]
struct Wire {
    bytes: Vec<u8>,
    /// The accused block's height.
    height: u64,
    /// Whether it counts for fork choice (see [`FORK_CHOICE_DEPTH`]).
    counted: bool,
}

const SEEDS: usize = 65536;
const PENDING: usize = 1024;

/// What [`Policy::offer`] did with a piece of evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    /// New, about a template whose key it is: stored and queued for relay.
    New,
    /// We already have evidence for that template.
    Known,
    /// Its signatures verify, but we don't know the template's key yet: kept aside, not relayed.
    Pending,
    /// The signatures don't verify, or the key is not the one the template's
    /// coinbase commits: forged (an honest node never relays such evidence).
    Invalid,
}

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
    outbox: Vec<Vec<u8>>,
    wires: Bounded<[u8; 32], Wire>,
    /// Where recent evidence is kept across restarts (None: memory only).
    file: Option<std::path::PathBuf>,
    /// Evidence about a seed whose template key we don't know yet: kept (a
    /// few), not relayed and not acted on, until [`Policy::note_key`] shows
    /// the key is the template's.
    pending: Bounded<[u8; 32], Evidence>,
    /// Evidence seen first (to relay once): counted for logs and tests.
    pub received: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Policy { claims: Bounded::new(SEEDS), keys: Bounded::new(SEEDS), evidence: Bounded::new(SEEDS), outbox: Vec::new(), wires: Bounded::new(WIRES), file: None, pending: Bounded::new(PENDING), received: 0 }
    }
}

impl Policy {
    /// A signature checked: template `seed` commits miner key `key`.
    pub fn note_key(&mut self, seed: &[u8; 32], key: &[u8; 32]) {
        self.keys.insert(*seed, *key);
        if let Some(e) = self.pending.remove(seed) {
            if e.key == *key {
                self.store(e);
            }
        }
    }

    /// A run whose signature checked (`key` is its coinbase's miner key):
    /// the first for its seed is the claim; any different one is
    /// equivocation, and its evidence is kept and queued for relay.
    pub fn claim(&mut self, seed: &[u8; 32], key: &[u8; 32], run: &SignedRun) -> Claim {
        // `key` is the one the block's coinbase commits (its signature checked against it).
        if self.keys.get(seed) != Some(key) {
            self.note_key(seed, key);
        }
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
        self.offer(e) == Offer::New
    }

    /// Evidence from anywhere (our own finding or a peer): stored and relayed
    /// only if it verifies and its key is the template's, as recorded when a
    /// block of that seed passed its signature check ([`Policy::note_key`]).
    /// Evidence keyed by seed alone, first come, would let anyone pre-empt the
    /// real evidence for a seed with their own key's (or spam relays).
    pub fn offer(&mut self, e: Evidence) -> Offer {
        let e = e.canonical();
        if !e.verify() {
            return Offer::Invalid;
        }
        match self.keys.get(&e.seed) {
            Some(k) if *k != e.key => Offer::Invalid,
            Some(_) if self.evidence.get(&e.seed).is_some() => Offer::Known,
            Some(_) => {
                self.store(e);
                Offer::New
            }
            None => {
                if self.pending.get(&e.seed).is_some() {
                    return Offer::Known;
                }
                self.pending.insert(e.seed, e);
                Offer::Pending
            }
        }
    }

    fn store(&mut self, e: Evidence) {
        self.evidence.insert(e.seed, e);
        self.received += 1;
        // Relayable only with its template (the daemon handles every block inside a block scope).
        if let Some((parts, top)) = scoped(&e.seed) {
            self.keep_wire(&e, &parts, top, None);
        }
    }

    fn keep_wire(&mut self, e: &Evidence, parts: &TemplateParts, top: u64, counted: Option<bool>) {
        let Ok(f) = template::facts(parts) else { return };
        let mut bytes = e.encode();
        bytes.extend_from_slice(&template::encode(parts));
        let counted = counted.unwrap_or(top.saturating_sub(f.height) <= FORK_CHOICE_DEPTH);
        self.wires.insert(e.seed, Wire { bytes: bytes.clone(), height: f.height, counted });
        if self.outbox.len() < 64 {
            self.outbox.push(bytes);
        }
        self.save();
    }

    /// Evidence from a peer, on the wire (self-contained, [`MAX_WIRE`]); `top`: our top height.
    pub fn offer_wire(&mut self, bytes: &[u8], top: u64) -> Offer {
        self.offer_wire_as(bytes, top, None)
    }

    fn offer_wire_as(&mut self, bytes: &[u8], top: u64, counted: Option<bool>) -> Offer {
        if bytes.len() <= EVIDENCE_LEN || bytes.len() > MAX_WIRE {
            return Offer::Invalid;
        }
        let Some(e) = Evidence::decode(&bytes[..EVIDENCE_LEN]) else { return Offer::Invalid };
        let Ok((parts, used)) = template::decode(&bytes[EVIDENCE_LEN..]) else { return Offer::Invalid };
        if EVIDENCE_LEN + used != bytes.len() || !e.verify() {
            return Offer::Invalid;
        }
        match template::facts(&parts) {
            Ok(f) if f.seed == e.seed && f.key == Some(e.key) => {}
            _ => return Offer::Invalid, // not that template, or not its key: forged
        }
        if matches!(self.keys.get(&e.seed), Some(k) if *k != e.key) {
            return Offer::Known; // our signature checks saw another key for this seed (a parser disagreement): ignore
        }
        if self.wires.get(&e.seed).is_some() {
            return Offer::Known;
        }
        self.keys.insert(e.seed, e.key);
        self.pending.remove(&e.seed);
        let e = e.canonical();
        self.evidence.insert(e.seed, e);
        self.received += 1;
        self.keep_wire(&e, &parts, top, counted);
        Offer::New
    }

    /// The wire evidence for `seed`, if any (sent along with the accused block).
    pub fn wire_for(&self, seed: &[u8; 32]) -> Option<Vec<u8>> {
        self.wires.get(seed).map(|w| w.bytes.clone())
    }

    /// Wire evidence whose accused block is at most [`RECENT_HEIGHTS`] below `top` (for a new peer).
    pub fn recent(&self, top: u64) -> Vec<Vec<u8>> {
        self.wires.values().filter(|w| w.height + RECENT_HEIGHTS >= top).map(|w| w.bytes.clone()).collect()
    }

    /// Fork choice with the equivocation penalty (relay / fork-choice policy, not consensus).
    /// `main` and `alt` are the two chains above their fork point: their lengths and their
    /// blocks' seeds. A chain with a block whose evidence counts ([`FORK_CHOICE_DEPTH`]) weighs
    /// 1.5 blocks less: an equivocated chain needs a lead of 2 to win, and a clean chain beats an
    /// equivocated one of equal length or one block longer. Otherwise the longer chain wins, and
    /// a tie keeps the chain we have. True: switch to `alt`.
    pub fn fork_choice(&self, main_len: u64, main: &[[u8; 32]], alt_len: u64, alt: &[[u8; 32]]) -> bool {
        let tainted = |s: &[[u8; 32]]| s.iter().any(|x| self.wires.get(x).is_some_and(|w| w.counted));
        let w = |len: u64, t: bool| 2 * len as i128 - if t { 3 } else { 0 };
        w(alt_len, tainted(alt)) > w(main_len, tainted(main))
    }

    /// Keeps recent evidence in `path` across restarts, loading what is there now.
    pub fn persist(&mut self, path: std::path::PathBuf) {
        if let Ok(b) = std::fs::read(&path) {
            let mut i = 0;
            while i + 5 <= b.len() {
                let counted = b[i] != 0;
                let n = u32::from_le_bytes(b[i + 1..i + 5].try_into().unwrap()) as usize;
                let Some(rec) = b.get(i + 5..i + 5 + n) else { break };
                let _ = self.offer_wire_as(rec, 0, Some(counted));
                i += 5 + n;
            }
            self.outbox.clear(); // relayed before the restart; new peers get it on connect
        }
        self.file = Some(path);
    }

    fn save(&self) {
        let Some(path) = &self.file else { return };
        let mut ws: Vec<&Wire> = self.wires.values().collect();
        ws.sort_by_key(|w| std::cmp::Reverse(w.height));
        let mut out = Vec::new();
        for w in ws.into_iter().take(64) {
            out.push(w.counted as u8);
            out.extend_from_slice(&(w.bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(&w.bytes);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &out).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// The next new evidence to relay, on the wire (only evidence with its template is relayed).
    pub fn take_outbox(&mut self) -> Option<Vec<u8>> {
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
        assert_eq!(p.evidence(&seed), Some(e));
        assert!(p.take_outbox().is_none(), "without its template (no block scope) it is kept, not relayed");
        // A third run, or the second again: the evidence is kept, relayed once.
        assert!(matches!(p.claim(&seed, &k.public, &run(&k, &seed, 3, 5000)), Claim::Equivocation(_)));
        assert!(matches!(p.claim(&seed, &k.public, &shorter), Claim::Equivocation(_)));
        
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
        assert_eq!(p.offer(own), Offer::Invalid, "the key isn't the template's: not stored, not relayed");
        assert!(!p.avoid(&seed) && p.take_outbox().is_none());
        // The real evidence still gets in afterwards (no pre-emption).
        let real = Evidence { seed, key: k.public, a: run(&k, &seed, 1, 5), b: run(&k, &seed, 2, 5) };
        assert_eq!(p.offer(real), Offer::New);
        assert!(p.avoid(&seed));
        // Evidence about a seed whose key we don't know yet waits, unrelayed, until a block shows the key.
        let mut q = Policy::default();
        let other = [8; 32];
        let early = Evidence { seed: other, key: k.public, a: run(&k, &other, 1, 5), b: run(&k, &other, 2, 5) };
        assert_eq!(q.offer(early), Offer::Pending);
        assert!(q.take_outbox().is_none() && !q.avoid(&other));
        q.note_key(&other, &k.public);
        assert!(q.avoid(&other), "promoted once the template's key is known");
        // Pending evidence with another key than the template's is dropped when the key shows up.
        let mut r = Policy::default();
        let att = Evidence { seed: other, key: attacker.public, a: run(&attacker, &other, 1, 5), b: run(&attacker, &other, 2, 5) };
        assert_eq!(r.offer(att), Offer::Pending);
        r.note_key(&other, &k.public);
        assert!(!r.avoid(&other) && r.take_outbox().is_none());
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
        
        assert!(p.avoid(&seed));
        assert_eq!(choose_parent(&p, &seed, &[seed]), Some(usize::MAX), "no clean sibling: build on the parent");
        assert_eq!(choose_parent(&p, &seed, &[seed, clean]), Some(1));
        assert_eq!(choose_parent(&p, &clean, &[seed]), None);
    }

    // A real v0.3 template (block 1 of a test network) and the secret of the key its coinbase commits.
    const TEMPLATE: &str = "1010cbc79cd806cbd56c9bf4ea17392ccfdb7a661f0e3e449b55b95a9a2e1eeea426132dfbde9f00000000020501ff01018080808080feff01039104d7da53e0617d793c43dc07cc05c930db0173bacd504f94efe2193b04dea93a44019bad870f0d0a9684b9bee687e585e4c3f70d3c93b3ce06effa436b9672cdda01022164343f896777063a980d337770fc0f035c0f7583eba9d1b1ed6be26b49032051bf00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
    const SECRET: &str = "f09cf5fe839a298afb6154783ae07874f9e61e9d9793d2ebb23fcb945f50290e";

    fn unhex(h: &str) -> Vec<u8> {
        (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect()
    }

    fn real() -> (Keypair, [u8; 32], Vec<u8>) {
        let k = Keypair::from_secret(&unhex(SECRET).try_into().unwrap()).unwrap();
        let blob = unhex(TEMPLATE);
        let seed = template::facts(&template::split_block(&blob).unwrap()).unwrap().seed;
        (k, seed, blob)
    }

    #[test]
    fn evidence_on_the_wire_carries_its_template_and_verifies_without_the_block() {
        let (k, seed, blob) = real();
        let mut p = Policy::default();
        {
            // The daemon handles the second run inside a block scope: the evidence gets the template and is relayed.
            let _s = block_scope(&blob, 1);
            assert_eq!(p.claim(&seed, &k.public, &run(&k, &seed, 1, 3000)), Claim::First);
            assert!(matches!(p.claim(&seed, &k.public, &run(&k, &seed, 2, 3000)), Claim::Equivocation(_)));
        }
        let wire = p.take_outbox().expect("relayed with its template");
        assert!(wire.len() > EVIDENCE_LEN && wire.len() <= MAX_WIRE);
        assert_eq!(p.wire_for(&seed), Some(wire.clone()));
        assert_eq!(p.recent(1), vec![wire.clone()]);
        assert!(p.recent(1 + RECENT_HEIGHTS + 1).is_empty());
        // A node that never saw the block accepts it, and only once.
        let mut q = Policy::default();
        assert_eq!(q.offer_wire(&wire, 1), Offer::New);
        assert!(q.avoid(&seed) && q.take_outbox().is_some());
        assert_eq!(q.offer_wire(&wire, 1), Offer::Known);
        // Someone else's key over this template, with this template attached: forged.
        let att = Keypair::from_entropy(&[9; 32]).unwrap();
        let mut forged = Evidence { seed, key: att.public, a: run(&att, &seed, 1, 5), b: run(&att, &seed, 2, 5) }.encode();
        forged.extend_from_slice(&wire[EVIDENCE_LEN..]);
        assert_eq!(Policy::default().offer_wire(&forged, 1), Offer::Invalid);
        // Real evidence with another template attached, truncated, or v1-sized: invalid.
        let mut other = wire.clone();
        let n = other.len();
        other[n - 40] ^= 1; // inside the coinbase
        assert_eq!(Policy::default().offer_wire(&other, 1), Offer::Invalid);
        assert_eq!(Policy::default().offer_wire(&wire[..wire.len() - 1], 1), Offer::Invalid);
        assert_eq!(Policy::default().offer_wire(&wire[..EVIDENCE_LEN], 1), Offer::Invalid);
    }

    #[test]
    fn fork_choice_penalises_a_chain_with_an_equivocated_block_both_ways_and_late_evidence_does_not_count() {
        let (k, seed, blob) = real();
        let clean = [[1u8; 32], [2; 32], [3; 32]];
        let mut p = Policy::default();
        // Without evidence: the longer chain, a tie keeps ours.
        assert!(p.fork_choice(1, &[seed], 2, &clean[..2]) && !p.fork_choice(1, &[seed], 1, &clean[..1]));
        let _s = block_scope(&blob, 2); // the accused block (height 1) at depth 1
        p.claim(&seed, &k.public, &run(&k, &seed, 1, 3000));
        p.claim(&seed, &k.public, &run(&k, &seed, 2, 3000));
        // Our chain has the equivocated block: a clean competitor of equal length, or one block shorter, wins.
        assert!(p.fork_choice(2, &[seed, [7; 32]], 2, &clean[..2]));
        assert!(p.fork_choice(2, &[seed, [7; 32]], 1, &clean[..1]));
        assert!(!p.fork_choice(3, &[seed, [7; 32], [8; 32]], 1, &clean[..1]));
        // The equivocated chain needs a lead of 2 to win.
        assert!(!p.fork_choice(1, &clean[..1], 2, &[seed, [7; 32]]));
        assert!(p.fork_choice(1, &clean[..1], 3, &[seed, [7; 32], [8; 32]]));
        // Evidence that arrives when the accused block is deeper than FORK_CHOICE_DEPTH: kept and relayed, not counted.
        let wire = p.wire_for(&seed).unwrap();
        let mut late = Policy::default();
        assert_eq!(late.offer_wire(&wire, 1 + FORK_CHOICE_DEPTH + 1), Offer::New);
        assert!(late.avoid(&seed));
        assert!(!late.fork_choice(2, &[seed, [7; 32]], 2, &clean[..2]), "late evidence: plain length");
        let mut in_time = Policy::default();
        assert_eq!(in_time.offer_wire(&wire, 1 + FORK_CHOICE_DEPTH), Offer::New);
        assert!(in_time.fork_choice(2, &[seed, [7; 32]], 2, &clean[..2]));
    }

    #[test]
    fn recent_evidence_survives_a_restart() {
        let (k, seed, blob) = real();
        let dir = std::env::temp_dir().join(format!("sm64ev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sm64_evidence.bin");
        let _ = std::fs::remove_file(&path);
        let mut p = Policy::default();
        p.persist(path.clone());
        {
            let _s = block_scope(&blob, 1);
            p.claim(&seed, &k.public, &run(&k, &seed, 1, 3000));
            p.claim(&seed, &k.public, &run(&k, &seed, 2, 3000));
        }
        let mut q = Policy::default();
        q.persist(path.clone());
        assert!(q.avoid(&seed) && q.wire_for(&seed).is_some());
        assert!(q.take_outbox().is_none(), "not re-flooded; sent to new peers on connect");
        assert!(q.fork_choice(1, &[seed], 1, &[[1; 32]]), "the fork-choice flag is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
