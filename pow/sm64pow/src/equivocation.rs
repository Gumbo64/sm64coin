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
    static BLOCK: RefCell<Option<Scoped>> = const { RefCell::new(None) };
}

/// While alive, evidence formed on this thread about `blob`'s template carries it (so it can be relayed).
#[derive(Clone)]
struct Scoped {
    /// None when the block doesn't fit template.rs's caps (then evidence about it is kept
    /// and counts here, but can't be relayed).
    parts: Option<TemplateParts>,
    seed: [u8; 32],
    height: u64,
    top: u64,
}

pub struct BlockScope(Option<Scoped>);
impl Drop for BlockScope {
    fn drop(&mut self) {
        let prev = self.0.take();
        BLOCK.with(|b| *b.borrow_mut() = prev);
    }
}
/// `blob`: a full block (its seed `seed`, at `height`); `top`: the height of our chain's top block.
pub fn block_scope(blob: &[u8], seed: [u8; 32], height: u64, top: u64) -> BlockScope {
    let parts = template::split_block(blob).ok().filter(|p| template::facts(p).is_ok_and(|f| f.seed == seed));
    let cur = Some(Scoped { parts, seed, height, top });
    BlockScope(BLOCK.with(|b| std::mem::replace(&mut *b.borrow_mut(), cur)))
}
fn scoped(seed: &[u8; 32]) -> Option<Scoped> {
    BLOCK.with(|b| b.borrow().as_ref().filter(|s| s.seed == *seed).cloned())
}

/// Whether evidence about a block at `height`, seen when our top is `top`, counts for fork
/// choice: the block at most FORK_CHOICE_DEPTH deep (a node far behind, syncing, doesn't count
/// evidence the network has long buried).
fn counts(height: u64, top: u64) -> bool {
    top >= height && top - height <= FORK_CHOICE_DEPTH
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
    /// Evidence formed here about a block too big for a wire: whether it counts for fork choice.
    local_counted: Bounded<[u8; 32], bool>,
    /// Where recent evidence is kept across restarts (None: memory only).
    file: Option<std::path::PathBuf>,
    /// Unsaved changes, and when the file was last written (saves are at most every 10 s: [`Policy::flush`]).
    dirty: bool,
    saved_at: f64,
    /// Evidence about a seed whose template key we don't know yet: kept (a
    /// few), not relayed and not acted on, until [`Policy::note_key`] shows
    /// the key is the template's.
    pending: Bounded<[u8; 32], Evidence>,
    /// Evidence seen first (to relay once): counted for logs and tests.
    pub received: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Policy { claims: Bounded::new(SEEDS), keys: Bounded::new(SEEDS), evidence: Bounded::new(SEEDS), outbox: Vec::new(), wires: Bounded::new(WIRES), local_counted: Bounded::new(WIRES), file: None, dirty: false, saved_at: f64::NEG_INFINITY, pending: Bounded::new(PENDING), received: 0 }
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
        if let Some(sc) = scoped(&e.seed) {
            match &sc.parts {
                Some(parts) => self.keep_wire(&e, parts, sc.top, None),
                // Too big for a wire (template.rs caps): it still counts here (an equivocator can't
                // dodge the penalty with a huge coinbase), it just isn't relayed.
                None => self.local_counted.insert(e.seed, counts(sc.height, sc.top)),
            }
        }
    }

    fn keep_wire(&mut self, e: &Evidence, parts: &TemplateParts, top: u64, counted: Option<bool>) {
        let Ok(f) = template::facts(parts) else { return };
        let mut bytes = e.encode();
        bytes.extend_from_slice(&template::encode(parts));
        let counted = counted.unwrap_or(counts(f.height, top));
        self.wires.insert(e.seed, Wire { bytes: bytes.clone(), height: f.height, counted });
        if self.outbox.len() < 64 {
            self.outbox.push(bytes);
        }
        self.dirty = true;
        self.flush(crate::admit::now());
    }

    /// Writes the file if something changed and the last write is 10 s old (called on every
    /// new piece and from the daemon's idle loop, through take_outbox), so a burst of evidence
    /// doesn't rewrite it each time.
    pub fn flush(&mut self, now: f64) {
        if self.dirty && now - self.saved_at >= 10.0 {
            self.save();
            self.dirty = false;
            self.saved_at = now;
        }
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
        // Plausible only for a block near our tip (anyone can invent a key, a template and two
        // runs): outside [top - RECENT_HEIGHTS, top + 2] it is ignored, not stored or relayed.
        // Restored evidence (persist) passes counted = Some(..) and is exempt.
        if counted.is_none() {
            let h = template::facts(&parts).map(|f| f.height).unwrap_or(0);
            if h + RECENT_HEIGHTS < top || h > top + 2 {
                return Offer::Known;
            }
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

    /// Wire evidence whose accused block is within [`RECENT_HEIGHTS`] of `top` (for a new peer).
    pub fn recent(&self, top: u64) -> Vec<Vec<u8>> {
        (0..).map_while(|i| self.recent_nth(top, i)).collect()
    }

    /// The `i`-th of [`Policy::recent`] (at most 64), without copying the others.
    pub fn recent_nth(&self, top: u64, i: usize) -> Option<Vec<u8>> {
        if i >= 64 {
            return None;
        }
        self.wires.values().filter(|w| w.height + RECENT_HEIGHTS >= top && w.height <= top + 2).nth(i).map(|w| w.bytes.clone())
    }

    /// Whether template `seed` has evidence that counts for fork choice.
    pub fn tainted(&self, seed: &[u8; 32]) -> bool {
        self.wires.get(seed).is_some_and(|w| w.counted) || self.local_counted.get(seed).copied().unwrap_or(false)
    }

    /// Fork choice with the equivocation penalty (relay / fork-choice policy, not consensus).
    /// `main` and `alt` are the two chains above their fork point: their lengths and their
    /// blocks' seeds. A chain with a block whose evidence counts ([`FORK_CHOICE_DEPTH`]) weighs
    /// 1.5 blocks less: an equivocated chain needs a lead of 2 to win, and a clean chain beats an
    /// equivocated one of equal length or one block longer. Otherwise the longer chain wins, and
    /// a tie keeps the chain we have. True: switch to `alt`.
    pub fn fork_choice(&self, main_len: u64, main: &[[u8; 32]], alt_len: u64, alt: &[[u8; 32]]) -> bool {
        // Only blocks right above the fork point (FORK_CHOICE_DEPTH + 1 of them) matter: the
        // penalty settles a race at the fork, it doesn't let an old taint decide a deep fork.
        let near = (FORK_CHOICE_DEPTH + 1) as usize;
        let tainted = |s: &[[u8; 32]]| s.iter().take(near).any(|x| self.tainted(x));
        let w = |len: u64, t: bool| 2 * len as i128 - if t { 3 } else { 0 };
        w(alt_len, tainted(alt)) > w(main_len, tainted(main))
    }

    /// [`Policy::fork_choice`] with proof weight (docs/V0.4-DESIGN.md 2.6): `main` and `alt` are
    /// the chains above the fork point as `(seed, counted proofs)` per block, nearest the fork
    /// first. With `proof_weight` on: records::prefer_alt, "longer AND heavier", a block weighs
    /// `1 + counted` and a tainted block 0, the length penalty as v0.3. A chain's blocks beyond
    /// the entries given (`len` larger than the slice) weigh 1 each. Off: exactly
    /// [`Policy::fork_choice`].
    pub fn fork_choice_weighted(&self, main_len: u64, main: &[([u8; 32], u32)], alt_len: u64, alt: &[([u8; 32], u32)]) -> bool {
        let seeds = |s: &[([u8; 32], u32)]| s.iter().map(|x| x.0).collect::<Vec<_>>();
        if !crate::features::active().proof_weight {
            return self.fork_choice(main_len, &seeds(main), alt_len, &seeds(alt));
        }
        let near = (FORK_CHOICE_DEPTH + 1) as usize;
        let side = |len: u64, s: &[([u8; 32], u32)]| crate::records::Side {
            len,
            weight: s.iter().filter(|(seed, _)| !self.tainted(seed)).map(|(_, c)| 1 + *c as u64).sum::<u64>() + len.saturating_sub(s.len() as u64),
            tainted: s.iter().take(near).any(|(seed, _)| self.tainted(seed)),
        };
        crate::records::prefer_alt(side(main_len, main), side(alt_len, alt), None)
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
        self.flush(crate::admit::now());
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
            let _s = block_scope(&blob, seed, 1, 1);
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
        let _s = block_scope(&blob, seed, 1, 2); // the accused block (height 1) at depth 1
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
            let _s = block_scope(&blob, seed, 1, 1);
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

    #[test]
    fn implausible_or_late_evidence_and_far_forks() {
        let (k, seed, blob) = real();
        let mut p = Policy::default();
        {
            let _s = block_scope(&blob, seed, 1, 1);
            p.claim(&seed, &k.public, &run(&k, &seed, 1, 3000));
            p.claim(&seed, &k.public, &run(&k, &seed, 2, 3000));
        }
        let wire = p.wire_for(&seed).unwrap();
        // Accused height 1: ignored by a node whose tip is far above it, or far below (not stored, not relayed).
        assert_eq!(Policy::default().offer_wire(&wire, 1 + RECENT_HEIGHTS + 1), Offer::Known);
        let mut syncing = Policy::default();
        assert_eq!(syncing.offer_wire(&wire, 0), Offer::New, "one below: still plausible");
        // A node behind the accused block doesn't count it for fork choice.
        assert!(!syncing.tainted(&seed));
        // The penalty only looks at blocks right above the fork point.
        let deep: Vec<[u8; 32]> = (0..5u8).map(|i| [i + 50; 32]).chain(std::iter::once(seed)).collect();
        assert!(!p.fork_choice(6, &deep, 6, &[[9; 32]; 6]), "a taint 6 blocks above the fork doesn't decide it");
        assert!(p.recent_nth(1, 0).is_some() && p.recent_nth(1, 1).is_none() && p.recent_nth(100, 0).is_none());
    }

    #[test]
    fn a_block_too_big_for_a_wire_still_counts_here() {
        let (k, seed, _) = real();
        let mut p = Policy::default();
        {
            let _s = block_scope(&[0u8; 10], seed, 5, 5); // not a parseable block: no template
            p.claim(&seed, &k.public, &run(&k, &seed, 1, 3000));
            p.claim(&seed, &k.public, &run(&k, &seed, 2, 3000));
        }
        assert!(p.wire_for(&seed).is_none() && p.take_outbox().is_none());
        assert!(p.tainted(&seed) && p.fork_choice(1, &[seed], 1, &[[1; 32]]));
    }

    /// Proof weight in fork choice (the default features: weight on): longer AND heavier; a tainted
    /// block weighs 0 and the length penalty stays. (The switch-off path is tests/stagec.rs.)
    #[test]
    fn weighted_fork_choice_taint_weighs_zero() {
        let mut p = Policy::default();
        let b = |i: u8, c: u32| ([i; 32], c);
        // alt longer and heavier: switch; longer but lighter: keep; heavier but not longer: keep
        assert!(p.fork_choice_weighted(2, &[b(1, 3), b(2, 3)], 3, &[b(3, 3), b(4, 3), b(5, 3)]));
        assert!(!p.fork_choice_weighted(2, &[b(1, 9), b(2, 9)], 3, &[b(3, 0), b(4, 0), b(5, 0)]));
        assert!(!p.fork_choice_weighted(3, &[b(1, 0), b(2, 0), b(6, 0)], 3, &[b(3, 9), b(4, 9), b(5, 9)]));
        // a tainted block of the alternative weighs 0 (its counted proofs vanish): 3 + 3 + 0 = 6 < main's 8
        p.local_counted.insert([5; 32], true);
        let main = [b(1, 3), b(2, 3)];
        let alt = [b(3, 3), b(4, 3), b(5, 90)];
        assert!(!p.fork_choice_weighted(2, &main, 3, &alt), "taint: weight 0 and the 1.5 block penalty (a lead of 2 is needed)");
        // untainted: weight 4 + 4 + 91 beats 8, length 3 > 2: switches
        let clean = Policy::default();
        assert!(clean.fork_choice_weighted(2, &main, 3, &alt));
        // entries fewer than the length: the rest weigh 1
        assert!(clean.fork_choice_weighted(1, &[b(1, 0)], 3, &[b(3, 0)]), "2 unknown blocks weigh 2: 3 > 1");
    }
}
