//! Included proofs (v0.4, layer 1, pure; the replay is chain.rs's `verify_records`): block h+1 carries the other valid signed runs for height h
//! as self-contained proof records, and fork choice weighs chains by them. docs/V0.4-DESIGN.md
//! section 2.
//!
//! ```text
//! record      = varint(len) header || varint(len) miner_tx || varint(tx_count) || branch (depth(tx_count) x 32)
//!               || varint(len) proof (a committed blob, commit.rs) || sig (64)
//! sm64_proofs = varint(n) || record_1 .. record_n
//! leaf        = keccak256("sm64prf1" || seed || input_hash || frames u32 LE || sig)
//! root        = tree_hash(leaves)          committed by the including block's coinbase extra nonce (tag 0x02)
//! ```
//!
//! The record bytes are a witness: the template header's timestamp (outside the seed) and the
//! proof's encoding may change in transit without changing the leaf. So a record set that doesn't
//! give the committed root, a template that doesn't hash to its leaf's seed, or a proof encoding
//! that doesn't decode to its committed inputs is a **bad witness** ([`RecordFault::BadWitness`]:
//! drop the sender, ask another peer), never an invalid block; only rules about what the leaves
//! commit make the block **invalid**.
//!
//! The coinbase extra nonce (v0.4): `0x64 || miner key || TLV*` with tag 0x01 (65 bytes: the
//! template's payout address, `flag || spend key || view key`, flag 1 = subaddress) and tag 0x02
//! (32 bytes: the included-proofs root), 0x04 (80 bytes: the VRF proof of the parent id, `vrf.rs`, only
//! for the optional `TieKey::Vrf`; 0x03 is reserved for the evidence burn). Unknown tags are ignored
//! (pools may add their own).
//!
//! Read next: payout.rs, then chain.rs (`verify_records`).

use crate::commit::{self, Header};
use crate::proof::Start;
use crate::sign::{self, cn_fast_hash};
use crate::template::{self, tree_hash};

/// Leaf domain tag.
pub const LEAF_TAG: &[u8; 8] = b"sm64prf1";
/// Extra-nonce TLV tags after the miner key.
pub const TLV_PAYOUT: u8 = 0x01;
pub const TLV_ROOT: u8 = 0x02;
/// 0x03 is reserved for the evidence burn (docs/V0.4-DESIGN.md section 5).
/// The VRF ticket of the optional `TieKey::Vrf` (vrf.rs): `gamma 32 || c 16 || s 32`, 80 bytes.
pub const TLV_VRF: u8 = 0x04;
/// Wire caps (docs/V0.4-DESIGN.md section 6): the whole field, one proof blob (the daemon's
/// sm64_proof cap), the branch depth.
pub const MAX_FIELD: usize = 2 << 20;
pub const MAX_PROOF: usize = 256 * 1024;
pub const MAX_RECORDS: usize = 256;

/// The v0.4 proof parameters (owner's decisions 2026-11-25, docs/V0.4-DESIGN.md section 7). The
/// windows are in **auction time**: a run is within `s` seconds of the winner if it would have
/// become valid at most `s` seconds after the winner's due time ([`window_frames`]), so they
/// follow the limit curve (and any future change of its slope) by themselves. The weight window
/// and the rewards are gates, to be set from the imitation-cost measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// At most this many records per block (decided: 64).
    pub cap: usize,
    /// The archive window, seconds of auction time after the winner's due time (decided: 60).
    pub archive_seconds: u64,
    /// The weight window (counted for fork choice), within the archive window; None: proofs weigh
    /// nothing and fork choice is v0.3's longest chain (default 60; to narrow, or turn off, from
    /// the imitation-cost result).
    pub weight_seconds: Option<u64>,
}

pub const PROPOSED: Window = Window { cap: 64, archive_seconds: 60, weight_seconds: Some(60) };

/// The first second after the parent at which a run of `frames` fits the limit at retarget
/// `difficulty` (its due time, as `hold.rs` sees it), or None if it never does.
pub fn due_elapsed(difficulty: u64, frames: u32) -> Option<u64> {
    let r = crate::rules::active();
    let (mut lo, mut hi) = (0u64, 1u64 << 40);
    if r.max_frames(difficulty, hi) < frames {
        return None;
    }
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if r.max_frames(difficulty, mid) >= frames {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Some(lo)
}

/// The longest run within `seconds` of auction time after a winner of `winner_frames` at
/// `difficulty`: the limit `seconds` after the winner's due time (deterministic from the
/// winner's frames and the parent's difficulty; the stamps don't enter, so restamping can't widen it).
pub fn window_frames(difficulty: u64, winner_frames: u32, seconds: u64) -> u32 {
    let r = crate::rules::active();
    match due_elapsed(difficulty, winner_frames) {
        Some(e) => r.max_frames(difficulty, e.saturating_add(seconds)).max(winner_frames),
        None => winner_frames,
    }
}

/// Each included proof is paid `min(base / divisor, base * pot_percent / 100 / n)` of new
/// issuance, on top of the miner's unchanged `base + fees` (decided: extra issuance, divisor 128).
/// The pot bounds the total (fairness-research V04.md: a total proof pot of about 10-25 % of the
/// block reward is about proportional near the floor; 50 % over-corrects); with 25 % and 128 it
/// binds above 32 proofs. Both are gates: None pays proofs nothing / leaves the total unbounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reward {
    pub divisor: Option<u64>,
    pub pot_percent: Option<u64>,
}

pub const PROPOSED_REWARD: Reward = Reward { divisor: Some(128), pot_percent: Some(25) };

/// What each of `n` included proofs is paid when the block's base reward is `base`.
pub fn proof_reward(base: u64, n: usize, r: &Reward) -> u64 {
    if !crate::features::active().proof_rewards {
        return 0;
    }
    let Some(d) = r.divisor else { return 0 };
    let per = base / d.max(1);
    match (r.pot_percent, n) {
        (Some(p), n) if n > 0 => per.min(((base as u128 * p as u128 / 100) / n as u128) as u64),
        _ => per,
    }
}

/// The most a coinbase with `n` included proofs may pay in all (None: overflow).
pub fn coinbase_total(base: u64, fees: u64, n: usize, r: &Reward) -> Option<u64> {
    base.checked_add(fees)?.checked_add(proof_reward(base, n, r).checked_mul(n as u64)?)
}

/// A payout address a template commits (Monero's standard address or subaddress public keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Payout {
    pub subaddress: bool,
    pub spend: [u8; 32],
    pub view: [u8; 32],
}

impl Payout {
    pub fn tlv(&self) -> Vec<u8> {
        let mut v = vec![TLV_PAYOUT, 65, self.subaddress as u8];
        v.extend_from_slice(&self.spend);
        v.extend_from_slice(&self.view);
        v
    }
}

/// What a v0.4 extra nonce commits after the key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NonceFields {
    pub payout: Option<Payout>,
    pub root: Option<[u8; 32]>,
    /// The VRF proof of the parent block id under the miner key (tag 0x04; structurally checked
    /// here, verified against the parent by `vrf::verify`).
    pub vrf: Option<crate::vrf::Proof>,
}

fn valid_point(k: &[u8; 32]) -> bool {
    use curve25519_dalek::edwards::CompressedEdwardsY;
    CompressedEdwardsY(*k).decompress().is_some_and(|p| p.compress().to_bytes() == *k && !p.is_small_order())
}

/// The TLVs of a v0.4 extra nonce (`0x64 || key || TLV*`). None: not a keyed nonce, or the bytes
/// after the key are not well-formed TLVs, or a known tag is malformed or repeated.
pub fn nonce_fields(nonce: &[u8]) -> Option<NonceFields> {
    if nonce.len() < sign::KEY_NONCE_LEN || nonce[0] != sign::KEY_TAG {
        return None;
    }
    let mut f = NonceFields::default();
    let mut i = sign::KEY_NONCE_LEN;
    while i < nonce.len() {
        let (&tag, &len) = (nonce.get(i)?, nonce.get(i + 1)?);
        let v = nonce.get(i + 2..i + 2 + len as usize)?;
        match tag {
            TLV_PAYOUT => {
                if len != 65 || v[0] > 1 || f.payout.is_some() {
                    return None;
                }
                let p = Payout { subaddress: v[0] == 1, spend: v[1..33].try_into().unwrap(), view: v[33..65].try_into().unwrap() };
                if !valid_point(&p.spend) || !valid_point(&p.view) {
                    return None;
                }
                f.payout = Some(p);
            }
            TLV_ROOT => {
                if len != 32 || f.root.is_some() {
                    return None;
                }
                f.root = Some(v.try_into().unwrap());
            }
            TLV_VRF => {
                if len as usize != crate::vrf::PROOF_LEN || f.vrf.is_some() {
                    return None;
                }
                let pi: crate::vrf::Proof = v.try_into().unwrap();
                if !crate::vrf::well_formed(&pi) {
                    return None;
                }
                f.vrf = Some(pi);
            }
            _ => {}
        }
        i += 2 + len as usize;
    }
    Some(f)
}

/// The extra nonce of a v0.4 template: key, payout, and the root when it includes proofs.
pub fn template_nonce(key: &[u8; 32], payout: Option<&Payout>, root: Option<&[u8; 32]>) -> Vec<u8> {
    template_nonce_vrf(key, payout, root, None)
}

/// [`template_nonce`] with the VRF proof of the parent id (tag 0x04, after the root).
pub fn template_nonce_vrf(key: &[u8; 32], payout: Option<&Payout>, root: Option<&[u8; 32]>, vrf: Option<&crate::vrf::Proof>) -> Vec<u8> {
    let mut n = sign::key_nonce(key).to_vec();
    if let Some(p) = payout {
        n.extend(p.tlv());
    }
    if let Some(r) = root {
        n.extend_from_slice(&[TLV_ROOT, 32]);
        n.extend_from_slice(r);
    }
    if let Some(pi) = vrf {
        n.extend_from_slice(&[TLV_VRF, crate::vrf::PROOF_LEN as u8]);
        n.extend_from_slice(pi);
    }
    n
}

// ---- the coinbase merkle branch ----

fn pair(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut x = [0u8; 64];
    x[..32].copy_from_slice(a);
    x[32..].copy_from_slice(b);
    cn_fast_hash(&x)
}

/// Monero's tree for `count` leaves (tree-hash.c): `cnt` (the largest power of two below
/// `count`, for count >= 3) and whether leaf 0 is paired at the first level (count a power of two).
fn shape(count: usize) -> (usize, bool) {
    let mut cnt = 2;
    while cnt < count {
        cnt <<= 1;
    }
    cnt >>= 1;
    (cnt, 2 * cnt == count)
}

/// How many sibling hashes the branch of leaf 0 has in a tree of `count` leaves.
pub fn depth(count: usize) -> usize {
    match count {
        0 | 1 => 0,
        2 => 1,
        n => {
            let (cnt, paired) = shape(n);
            cnt.trailing_zeros() as usize + paired as usize
        }
    }
}

/// The merkle branch of leaf 0 (the coinbase) in Monero's `tree_hash` of `leaves`.
pub fn coinbase_branch(leaves: &[[u8; 32]]) -> Vec<[u8; 32]> {
    match leaves.len() {
        0 | 1 => vec![],
        2 => vec![leaves[1]],
        count => {
            let (mut cnt, paired) = shape(count);
            let k = 2 * cnt - count;
            let mut ints = vec![[0u8; 32]; cnt];
            ints[..k].copy_from_slice(&leaves[..k]);
            let mut i = k;
            for j in k..cnt {
                ints[j] = pair(&leaves[i], &leaves[i + 1]);
                i += 2;
            }
            let mut out = Vec::new();
            if paired {
                out.push(leaves[1]);
            }
            while cnt > 2 {
                out.push(ints[1]);
                cnt >>= 1;
                for j in 0..cnt {
                    ints[j] = pair(&ints[2 * j], &ints[2 * j + 1]);
                }
            }
            out.push(ints[1]);
            out
        }
    }
}

/// The root from leaf 0 and its branch (leaf 0 is always the left child).
pub fn root_from_branch(leaf0: &[u8; 32], branch: &[[u8; 32]]) -> [u8; 32] {
    branch.iter().fold(*leaf0, |x, s| pair(&x, s))
}

// ---- records ----

/// One proof record as carried on the wire (a witness: see the module doc).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub header: Vec<u8>,
    pub miner_tx: Vec<u8>,
    pub tx_count: u64,
    pub branch: Vec<[u8; 32]>,
    pub proof: Vec<u8>,
    pub sig: [u8; 64],
}

/// What a record commits, once parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    pub seed: [u8; 32],
    pub key: [u8; 32],
    pub payout: Payout,
    pub prev_id: [u8; 32],
    pub height: u64,
    pub run: Header,
}

impl Facts {
    pub fn leaf(&self, sig: &[u8; 64]) -> [u8; 32] {
        leaf(&self.seed, &self.run.input_hash, self.run.frames, sig)
    }
}

pub fn leaf(seed: &[u8; 32], input_hash: &[u8; 32], frames: u32, sig: &[u8; 64]) -> [u8; 32] {
    let mut b = Vec::with_capacity(8 + 32 + 32 + 4 + 64);
    b.extend_from_slice(LEAF_TAG);
    b.extend_from_slice(seed);
    b.extend_from_slice(input_hash);
    b.extend_from_slice(&frames.to_le_bytes());
    b.extend_from_slice(sig);
    cn_fast_hash(&b)
}

/// The included-proofs root of `leaves` (32 zero bytes for none).
pub fn root(leaves: &[[u8; 32]]) -> [u8; 32] {
    tree_hash(leaves)
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

const E: &str = "malformed proof record";

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let end = self.p.checked_add(n).filter(|&e| e <= self.b.len()).ok_or(E)?;
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn varint(&mut self) -> Result<u64, &'static str> {
        let mut v = 0u64;
        for i in 0..10 {
            let c = *self.take(1)?.first().unwrap();
            v |= ((c & 0x7f) as u64) << (7 * i);
            if c & 0x80 == 0 {
                if c == 0 && i > 0 {
                    return Err(E); // canonical only
                }
                return Ok(v);
            }
        }
        Err(E)
    }
    fn bytes(&mut self, max: usize) -> Result<&'a [u8], &'static str> {
        let n = self.varint()? as usize;
        if n > max {
            return Err(E);
        }
        self.take(n)
    }
}

impl Record {
    pub fn encode_into(&self, o: &mut Vec<u8>) {
        put_varint(o, self.header.len() as u64);
        o.extend_from_slice(&self.header);
        put_varint(o, self.miner_tx.len() as u64);
        o.extend_from_slice(&self.miner_tx);
        put_varint(o, self.tx_count);
        for h in &self.branch {
            o.extend_from_slice(h);
        }
        put_varint(o, self.proof.len() as u64);
        o.extend_from_slice(&self.proof);
        o.extend_from_slice(&self.sig);
    }

    fn decode_from(r: &mut Rd) -> Result<Record, &'static str> {
        let header = r.bytes(template::MAX_HEADER)?.to_vec();
        let miner_tx = r.bytes(template::MAX_MINER_TX)?.to_vec();
        let tx_count = r.varint()?;
        if tx_count == 0 || tx_count > template::MAX_TX_HASHES as u64 + 1 {
            return Err(E);
        }
        let branch = (0..depth(tx_count as usize)).map(|_| r.take(32).map(|h| h.try_into().unwrap())).collect::<Result<Vec<_>, _>>()?;
        let proof = r.bytes(MAX_PROOF)?.to_vec();
        let sig = r.take(64)?.try_into().unwrap();
        Ok(Record { header, miner_tx, tx_count, branch, proof, sig })
    }

    /// The record of a template (header, coinbase, the other transactions' hashes), its committed
    /// proof and signature.
    pub fn new(parts: &template::TemplateParts, proof: Vec<u8>, sig: [u8; 64]) -> Result<Record, &'static str> {
        let mut leaves = vec![template::coinbase_hash(&parts.miner_tx)?];
        leaves.extend_from_slice(&parts.tx_hashes);
        Ok(Record { header: parts.header.clone(), miner_tx: parts.miner_tx.clone(), tx_count: leaves.len() as u64, branch: coinbase_branch(&leaves), proof, sig })
    }

    /// The record's leaf: needs only its template's seed and its proof header (not the key or
    /// the payout, so an authentic record that lacks them is invalid, not a bad witness).
    pub fn leaf(&self) -> Result<[u8; 32], &'static str> {
        let cb = template::coinbase_hash(&self.miner_tx)?;
        let t = template::facts_from_root(&self.header, &self.miner_tx, &root_from_branch(&cb, &self.branch), self.tx_count)?;
        let run = commit::header(&self.proof).map_err(|_| "the record's proof header does not parse")?;
        Ok(leaf(&t.seed, &run.input_hash, run.frames, &self.sig))
    }

    /// Parses what the record commits. Err: it is malformed, or its template commits no key or
    /// no payout address (an honest prover's template always has both: the record is invalid).
    pub fn facts(&self) -> Result<Facts, &'static str> {
        let cb = template::coinbase_hash(&self.miner_tx)?;
        let root = root_from_branch(&cb, &self.branch);
        let t = template::facts_from_root(&self.header, &self.miner_tx, &root, self.tx_count)?;
        let key = t.key.ok_or("the template commits no miner key")?;
        let nonce = template::coinbase_nonce(&self.miner_tx)?.ok_or("the template commits no miner key")?;
        let payout = nonce_fields(&nonce).and_then(|f| f.payout).ok_or("the template commits no payout address")?;
        let run = commit::header(&self.proof).map_err(|_| "the record's proof header does not parse")?;
        Ok(Facts { seed: t.seed, key, payout, prev_id: t.prev_id, height: t.height, run })
    }
}

/// One record alone (no count prefix: `Record::encode_into`'s bytes), the whole slice.
pub fn decode_one(b: &[u8]) -> Result<Record, &'static str> {
    if b.len() > MAX_FIELD {
        return Err(E);
    }
    let mut r = Rd { b, p: 0 };
    let rec = Record::decode_from(&mut r)?;
    if r.p != b.len() {
        return Err(E);
    }
    Ok(rec)
}

/// `varint(n) || records`.
pub fn encode(records: &[Record]) -> Vec<u8> {
    let mut o = Vec::new();
    put_varint(&mut o, records.len() as u64);
    for r in records {
        r.encode_into(&mut o);
    }
    o
}

/// Inverse of [`encode`] (the whole field: no trailing bytes).
pub fn decode(b: &[u8]) -> Result<Vec<Record>, &'static str> {
    if b.len() > MAX_FIELD {
        return Err(E);
    }
    let mut r = Rd { b, p: 0 };
    let n = r.varint()? as usize;
    if n > MAX_RECORDS {
        return Err(E);
    }
    let out = (0..n).map(|_| Record::decode_from(&mut r)).collect::<Result<Vec<_>, _>>()?;
    if r.p != b.len() {
        return Err(E);
    }
    Ok(out)
}

/// Why a block's records don't verify.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordFault {
    /// The witness bytes don't match what the block commits: drop them (and the sender), never
    /// the block.
    BadWitness(String),
    /// The committed records break a rule: the block is invalid.
    Invalid(String),
}

/// The block whose height the records are proofs for (block h, the including block's parent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Winner {
    pub height: u64,
    /// Block h's parent (every proof's template builds on it).
    pub prev_id: [u8; 32],
    /// Block h's miner key.
    pub key: [u8; 32],
    /// Block h's frames.
    pub frames: u32,
    /// Block h's retarget difficulty (the windows are in auction time at it).
    pub difficulty: u64,
    /// Block h's committed input hash (a record may not repeat the winner's run).
    pub input_hash: [u8; 32],
}

/// The structural checks of a block's records (docs/V0.4-DESIGN.md section 2.4, rules 1-8;
/// everything but the replay, which needs the parent's end state: [`crate::chain`]). `root` is
/// what the including block's coinbase commits (None: no TLV, i.e. no records). `start` is the
/// height's natural start (power-on iff block h's parent is the genesis block or ends the
/// cycle). Returns each record's facts and how many count for weight.
pub fn check(records: &[Record], root: Option<&[u8; 32]>, winner: &Winner, start: Start, w: &Window) -> Result<(Vec<Facts>, usize), RecordFault> {
    use RecordFault::*;
    let committed = root.copied().unwrap_or([0; 32]);
    let feats = crate::features::active();
    if !feats.included_proofs {
        // Stage A (features.rs): no records, and no root.
        return if records.is_empty() && committed == [0; 32] { Ok((vec![], 0)) } else { Err(Invalid("included proofs are not active".into())) };
    }
    if records.is_empty() {
        return if committed == [0; 32] { Ok((vec![], 0)) } else { Err(BadWitness("the block commits proofs but carries none".into())) };
    }
    // Authenticity first: the leaves must give the committed root.
    let mut leaves = Vec::with_capacity(records.len());
    for r in records {
        leaves.push(r.leaf().map_err(|e| BadWitness(format!("record does not parse: {e}")))?);
    }
    if self::root(&leaves) != committed {
        return Err(BadWitness("the records do not give the committed root".into()));
    }
    // Authentic: from here on every fault is the block's.
    let facts = records.iter().enumerate().map(|(i, r)| r.facts().map_err(|e| Invalid(format!("proof {i}: {e}")))).collect::<Result<Vec<Facts>, _>>()?;
    if records.len() > w.cap {
        return Err(Invalid(format!("{} proofs, over the cap of {}", records.len(), w.cap)));
    }
    let archive = window_frames(winner.difficulty, winner.frames, w.archive_seconds);
    let weight = w.weight_seconds.filter(|_| feats.proof_weight).map(|s| window_frames(winner.difficulty, winner.frames, s.min(w.archive_seconds)));
    let mut keys = std::collections::HashSet::new();
    let mut counted = 0;
    let mut last: Option<(u32, [u8; 32])> = None;
    let mut runs = std::collections::HashSet::new();
    for (i, (r, f)) in records.iter().zip(&facts).enumerate() {
        let bad = |why: &str| Invalid(format!("proof {i}: {why}"));
        if f.height != winner.height || f.prev_id != winner.prev_id {
            return Err(bad("not a template for the parent's height on the parent's parent"));
        }
        if f.key == winner.key || !keys.insert(f.key) {
            return Err(bad("one proof per key (and not the winner's key)"));
        }
        // Canonical order by an ungrindable key (one ticket per template: the seed's hash), as the
        // tie-break (pof::tie_of); keys are distinct, so seeds are, so the order is strict.
        let order = (f.run.frames, cn_fast_hash(&f.seed));
        if last.is_some_and(|l| l >= order) {
            return Err(bad("records must be in strictly increasing (frames, seed hash) order"));
        }
        last = Some(order);
        // One run counts once: an identical run on another seed (runs are seed-bound, so this is
        // all but impossible honestly) is a duplicate.
        if f.run.input_hash == winner.input_hash || !runs.insert(f.run.input_hash) {
            return Err(bad("the same run twice (input hash)"));
        }
        if !sign::verify(&f.key, &sign::message(&f.seed, &f.run.input_hash, f.run.frames), &r.sig) {
            return Err(bad("the signature does not verify with the template's key"));
        }
        if f.run.frames == 0 || f.run.frames > archive || f.run.frames > crate::rules::active().max_frames_any() {
            return Err(bad("outside the proof window"));
        }
        if f.run.start != start {
            return Err(bad("a proof starts where the height starts (no restarts)"));
        }
        counted += weight.is_some_and(|wf| f.run.frames <= wf) as usize;
    }
    Ok((facts, counted))
}

// ---- fork choice ----

/// One side of a fork, above the fork point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Side {
    pub len: u64,
    /// Blocks plus counted proofs (a tainted block weighs 0: docs/V0.4-DESIGN.md section 2.6).
    pub weight: u64,
    /// A block near the fork point has counted equivocation evidence (v0.3's penalty).
    pub tainted: bool,
}

/// Fork choice (node policy, deterministic from the chain): switch from `main` to `alt` iff `alt`
/// is longer (a tainted side's length counts 1.5 blocks less, as v0.3) AND heavier. With
/// `heal_depth` (OPEN, default off), once both sides are at least that many blocks deep the
/// heavier side wins alone. Ties keep `main`. The reorg cap (reorg.rs) is applied by the caller.
pub fn prefer_alt(main: Side, alt: Side, heal_depth: Option<u64>) -> bool {
    if !crate::features::active().proof_weight {
        // Proof weight off (features.rs): v0.3's rule, longer wins (with its taint penalty).
        let l = |s: Side| 2 * s.len as i128 - if s.tainted { 3 } else { 0 };
        return l(alt) > l(main);
    }
    let l = |s: Side| 2 * s.len as i128 - if s.tainted { 3 } else { 0 };
    let heavier = alt.weight > main.weight;
    (l(alt) > l(main) && heavier) || heal_depth.is_some_and(|d| main.len.min(alt.len) >= d && heavier)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::proof::{Proof, RunLength};
    use crate::sign::Keypair;
    use sm64_binds::GamePad;

    pub(crate) const PREV: [u8; 32] = [0xab; 32];
    pub(crate) const D: u64 = 1 << 16; // about where the 20-minute retarget settles

    pub(crate) fn varint(o: &mut Vec<u8>, v: u64) {
        put_varint(o, v)
    }

    /// A v2 coinbase (one tagged-key output) whose extra carries a tx pubkey and `nonce`.
    pub(crate) fn coinbase(height: u64, nonce: &[u8]) -> Vec<u8> {
        let mut t = vec![2];
        varint(&mut t, height + 72);
        t.extend_from_slice(&[1, 0xff]);
        varint(&mut t, height);
        t.push(1);
        varint(&mut t, 1_000_000_000);
        t.push(0x03);
        t.extend_from_slice(&[0x11; 33]);
        let mut extra = vec![0x01];
        extra.extend_from_slice(&[0x22; 32]);
        extra.push(0x02);
        varint(&mut extra, nonce.len() as u64);
        extra.extend_from_slice(nonce);
        varint(&mut t, extra.len() as u64);
        t.extend(extra);
        t.push(0);
        t
    }

    pub(crate) fn header(prev: &[u8; 32], ts: u64, nonce: u32) -> Vec<u8> {
        let mut h = vec![16, 16];
        varint(&mut h, ts);
        h.extend_from_slice(prev);
        h.extend_from_slice(&nonce.to_le_bytes());
        h
    }

    pub(crate) fn payout(b: u8) -> Payout {
        let k = |x: u8| {
            let mut e = [7u8; 32];
            e[0] = x;
            Keypair::from_entropy(&e).unwrap().public
        };
        Payout { subaddress: b % 2 == 1, spend: k(b), view: k(b.wrapping_add(100)) }
    }

    pub(crate) fn run(frames: u32, tag: u16) -> Proof {
        Proof { frames, start: Start::Parent, inputs: vec![GamePad::new(tag, 0, 0); frames as usize - 10] }
    }

    /// A record for height `h` on `prev` by key `k`, with `others` extra transactions.
    pub(crate) fn record(k: &Keypair, h: u64, prev: &[u8; 32], pr: &Proof, others: usize) -> (Record, Facts) {
        let mtx = coinbase(h, &template_nonce(&k.public, Some(&payout(k.public[0])), None));
        let parts = template::TemplateParts { header: header(prev, 1_800_000_000, 7), miner_tx: mtx, tx_hashes: (0..others).map(|i| [i as u8 + 1; 32]).collect() };
        let seed = template::facts(&parts).unwrap().seed;
        let blob = commit::commit_with(&RunLength, pr);
        let sig = k.sign_run(&seed, &commit::proof_id(&blob), pr.frames);
        let r = Record::new(&parts, blob, sig).unwrap();
        let f = r.facts().unwrap();
        assert_eq!(f.seed, seed, "the branch gives the template's real seed");
        (r, f)
    }

    pub(crate) fn winner() -> (Keypair, Winner) {
        let k = Keypair::from_entropy(&[200; 32]).unwrap();
        (k.clone(), Winner { height: 5, prev_id: PREV, key: k.public, frames: 2000, difficulty: D, input_hash: [0xcc; 32] })
    }

    pub(crate) fn set(n: usize) -> (Vec<Record>, [u8; 32]) {
        let mut v: Vec<(Record, Facts)> = (0..n).map(|i| record(&Keypair::from_entropy(&[i as u8 + 1; 32]).unwrap(), 5, &PREV, &run(2000 + 37 * i as u32, i as u16), i % 4)).collect();
        v.sort_by_key(|(_, f)| (f.run.frames, cn_fast_hash(&f.seed)));
        let leaves: Vec<[u8; 32]> = v.iter().map(|(r, f)| f.leaf(&r.sig)).collect();
        (v.into_iter().map(|(r, _)| r).collect(), root(&leaves))
    }

    #[test]
    fn the_branch_gives_monero_tree_hash_for_every_count() {
        for count in 1..=70usize {
            let leaves: Vec<[u8; 32]> = (0..count).map(|i| cn_fast_hash(&(i as u64).to_le_bytes())).collect();
            let b = coinbase_branch(&leaves);
            assert_eq!(b.len(), depth(count), "depth {count}");
            assert_eq!(root_from_branch(&leaves[0], &b), tree_hash(&leaves), "root {count}");
        }
    }

    #[test]
    fn the_vrf_tlv_parses_strictly() {
        let k = Keypair::from_entropy(&[3; 32]).unwrap();
        let p = payout(4);
        let pi = crate::vrf::prove(&k, &[7; 32]);
        let n = template_nonce_vrf(&k.public, Some(&p), Some(&[9; 32]), Some(&pi));
        assert!(n.len() <= 255, "fits the extra nonce: {}", n.len());
        let f = nonce_fields(&n).unwrap();
        assert_eq!((f.vrf, f.payout, f.root), (Some(pi), Some(p), Some([9; 32])));
        assert_eq!(template_nonce(&k.public, Some(&p), None), template_nonce_vrf(&k.public, Some(&p), None, None));
        let mut twice = n.clone();
        twice.extend_from_slice(&[TLV_VRF, 80]);
        twice.extend_from_slice(&pi);
        assert_eq!(nonce_fields(&twice), None, "repeated");
        let mut short = template_nonce(&k.public, None, None);
        short.extend_from_slice(&[TLV_VRF, 79]);
        short.extend_from_slice(&pi[..79]);
        assert_eq!(nonce_fields(&short), None, "wrong length");
        let mut bad_s = template_nonce_vrf(&k.public, None, None, Some(&pi));
        let l = bad_s.len();
        bad_s[l - 32..].fill(0xff);
        assert_eq!(nonce_fields(&bad_s), None, "s not canonical");
        let mut reserved = template_nonce(&k.public, None, None);
        reserved.extend_from_slice(&[0x03, 1, 0]);
        assert_eq!(nonce_fields(&reserved).unwrap().vrf, None, "0x03 stays unknown (evidence burn)");
    }

    #[test]
    fn nonce_tlvs() {
        let k = Keypair::from_entropy(&[3; 32]).unwrap();
        let p = payout(4);
        let n = template_nonce(&k.public, Some(&p), Some(&[9; 32]));
        assert!(n.len() <= 255);
        assert_eq!(nonce_fields(&n), Some(NonceFields { payout: Some(p), root: Some([9; 32]), vrf: None }));
        assert_eq!(sign::miner_key(&n), Some(k.public), "v0.3's key parsing still reads the key");
        let mut pool = n.clone();
        pool.extend_from_slice(&[0xf0, 3, 1, 2, 3]);
        assert_eq!(nonce_fields(&pool).unwrap().payout, Some(p), "unknown tags are skipped");
        let mut trunc = n.clone();
        trunc.pop();
        assert_eq!(nonce_fields(&trunc), None);
        let mut twice = n.clone();
        twice.extend(p.tlv());
        assert_eq!(nonce_fields(&twice), None, "a repeated payout");
        let mut badkey = template_nonce(&k.public, Some(&Payout { spend: [0; 32], ..p }), None);
        assert_eq!(nonce_fields(&badkey), None, "not a point");
        badkey.truncate(33);
        assert_eq!(nonce_fields(&badkey), Some(NonceFields::default()), "a v0.3 nonce: no fields");
    }

    #[test]
    fn records_round_trip_and_never_panic_on_truncation() {
        let (rs, _) = set(5);
        let e = encode(&rs);
        assert_eq!(decode(&e).unwrap(), rs);
        for n in 0..e.len() {
            assert!(decode(&e[..n]).is_err(), "truncated {n}");
        }
        let mut tail = e.clone();
        tail.push(0);
        assert!(decode(&tail).is_err());
        assert_eq!(decode(&encode(&[])).unwrap(), vec![]);
    }

    #[test]
    fn a_good_set_passes_and_counts_the_weight_window() {
        let (_, w) = winner();
        let (rs, root) = set(8);
        let (facts, counted) = check(&rs, Some(&root), &w, Start::Parent, &PROPOSED).unwrap();
        assert_eq!(facts.len(), 8);
        // frames 2000 + 37 i (<= 2259): all within 60 s of auction time (about +17 %)
        assert_eq!(counted, 8);
        let w10 = window_frames(D, 2000, 10);
        let narrow = Window { weight_seconds: Some(10), ..PROPOSED };
        assert_eq!(check(&rs, Some(&root), &w, Start::Parent, &narrow).unwrap().1, facts.iter().filter(|f| f.run.frames <= w10).count());
        assert!(w10 < 2259 && w10 >= 2037, "10 s: a few percent ({w10})");
        let off = Window { weight_seconds: None, ..PROPOSED };
        assert_eq!(check(&rs, Some(&root), &w, Start::Parent, &off).unwrap().1, 0, "the weight gate off");
        assert_eq!(check(&[], None, &w, Start::Parent, &PROPOSED).unwrap(), (vec![], 0));
    }

    #[test]
    fn witness_faults_are_not_invalid_blocks() {
        let (_, w) = winner();
        let (rs, root) = set(4);
        let bw = |r: Result<(Vec<Facts>, usize), RecordFault>| matches!(r, Err(RecordFault::BadWitness(_)));
        assert!(bw(check(&rs, Some(&[1; 32]), &w, Start::Parent, &PROPOSED)), "another root");
        assert!(bw(check(&rs[..3], Some(&root), &w, Start::Parent, &PROPOSED)), "a record dropped");
        assert!(bw(check(&[], Some(&root), &w, Start::Parent, &PROPOSED)), "records stripped");
        let mut t = rs.clone();
        t[1].miner_tx[10] ^= 1; // another template: another seed, another leaf
        assert!(bw(check(&t, Some(&root), &w, Start::Parent, &PROPOSED)));
        let mut t = rs.clone();
        t[2].sig[5] ^= 1;
        assert!(bw(check(&t, Some(&root), &w, Start::Parent, &PROPOSED)), "the leaf commits the signature");
        // The witness may change where the leaf doesn't look: the header timestamp, the encoding.
        let mut t = rs.clone();
        t[0].header = header(&PREV, 1_800_000_999, 7);
        let hdr = commit::header(&t[3].proof).unwrap();
        t[3].proof = commit::join(&hdr, &[0, 1, 2, 3, 4]); // a (bad) raw encoding: the replay's business
        assert!(check(&t, Some(&root), &w, Start::Parent, &PROPOSED).is_ok());
    }

    #[test]
    fn rule_violations_are_invalid() {
        let (wk, w) = winner();
        let inv = |rs: &[Record], start: Start, win: &Window| {
            let leaves: Vec<[u8; 32]> = rs.iter().map(|r| r.leaf().unwrap()).collect();
            matches!(check(rs, Some(&root(&leaves)), &w, start, win), Err(RecordFault::Invalid(_)))
        };
        let k1 = Keypair::from_entropy(&[1; 32]).unwrap();
        let k2 = Keypair::from_entropy(&[2; 32]).unwrap();
        let a = record(&k1, 5, &PREV, &run(2100, 1), 0).0;
        let b = record(&k2, 5, &PREV, &run(2200, 2), 0).0;
        assert!(!inv(&[a.clone(), b.clone()], Start::Parent, &PROPOSED));
        assert!(inv(&[b.clone(), a.clone()], Start::Parent, &PROPOSED), "order");
        // The same run under two keys counts once (and may not repeat the winner's run).
        let mut same = [record(&k1, 5, &PREV, &run(2100, 9), 0).0, record(&k2, 5, &PREV, &run(2100, 9), 0).0];
        same.sort_by_key(|r| cn_fast_hash(&r.facts().unwrap().seed));
        assert!(inv(&same, Start::Parent, &PROPOSED), "dedupe by input hash");
        let ws = record(&k1, 5, &PREV, &run(2100, 8), 0).0;
        let w2 = Winner { input_hash: ws.facts().unwrap().run.input_hash, ..w };
        let leaves = [ws.leaf().unwrap()];
        assert!(matches!(check(&[ws], Some(&root(&leaves)), &w2, Start::Parent, &PROPOSED), Err(RecordFault::Invalid(_))), "the winner's run");
        assert!(inv(&[a.clone(), a.clone()], Start::Parent, &PROPOSED), "duplicate");
        let a2 = record(&k1, 5, &PREV, &run(2300, 3), 1).0;
        assert!(inv(&[a.clone(), a2], Start::Parent, &PROPOSED), "one proof per key");
        assert!(inv(&[record(&wk, 5, &PREV, &run(2100, 4), 0).0], Start::Parent, &PROPOSED), "the winner's own key");
        assert!(inv(&[record(&k1, 6, &PREV, &run(2100, 1), 0).0], Start::Parent, &PROPOSED), "another height");
        assert!(inv(&[record(&k1, 5, &[1; 32], &run(2100, 1), 0).0], Start::Parent, &PROPOSED), "another parent");
        let top = window_frames(D, 2000, 60);
        assert!(inv(&[record(&k1, 5, &PREV, &run(top + 1, 1), 0).0], Start::Parent, &PROPOSED), "after the archive window");
        assert!(!inv(&[record(&k1, 5, &PREV, &run(top, 1), 0).0], Start::Parent, &PROPOSED), "at its edge");
        assert!(!inv(&[record(&k1, 5, &PREV, &run(1500, 1), 0).0], Start::Parent, &PROPOSED), "shorter than the winner is fine");
        assert!(inv(&[a.clone()], Start::PowerOn, &PROPOSED), "the height's natural start");
        assert!(inv(&[a.clone(), b.clone()], Start::Parent, &Window { cap: 1, ..PROPOSED }), "cap");
        // A signature by another key over the right message: committed by the leaf, so invalid.
        let mut forged = a.clone();
        let f = forged.facts().unwrap();
        forged.sig = k2.sign_run(&f.seed, &f.run.input_hash, f.run.frames);
        assert!(inv(&[forged], Start::Parent, &PROPOSED), "signature");
        // A template without a payout address doesn't parse as a record.
        let parts = template::TemplateParts { header: header(&PREV, 1, 7), miner_tx: coinbase(5, &k1.extra_nonce()), tx_hashes: vec![] };
        let seed = template::facts(&parts).unwrap().seed;
        let nopay = Record::new(&parts, a.proof.clone(), k1.sign_run(&seed, &commit::proof_id(&a.proof), 2100)).unwrap();
        assert!(nopay.facts().is_err());
        let l = nopay.leaf().unwrap();
        assert!(matches!(check(&[nopay], Some(&root(&[l])), &w, Start::Parent, &PROPOSED), Err(RecordFault::Invalid(_))), "authentic but no payout: invalid");
    }

    #[test]
    fn rewards_come_on_top_of_the_miners_and_the_pot_bounds_them() {
        let base = 1_125_800_000_000_000u64; // ~1125.8 SM64, v0.3's first blocks
        let r = PROPOSED_REWARD;
        assert_eq!(proof_reward(base, 1, &r), base / 128);
        assert_eq!(proof_reward(base, 32, &r), base / 128, "the pot binds only above 32 proofs");
        assert_eq!(coinbase_total(base, 7, 0, &r), Some(base + 7), "no proofs: v0.3's coinbase");
        assert_eq!(coinbase_total(base, 0, 64, &r), Some(base + 64 * (base / 4 / 64)), "the cap: the pot, 25 %");
        let open = Reward { pot_percent: None, ..r };
        assert_eq!(coinbase_total(base, 0, 64, &open), Some(base + 64 * (base / 128)), "no pot: +50 % at the cap");
        assert_eq!(coinbase_total(base, 0, 64, &Reward { divisor: None, ..r }), Some(base), "the reward gate off");
        assert_eq!(coinbase_total(u64::MAX, 1, 0, &r), None);
    }

    #[test]
    fn windows_are_in_auction_time() {
        let r = crate::rules::active();
        for (d, f) in [(1u64, 900u32), (1, 30_000), (D, 2000), (D, 2500), (1 << 24, 700)] {
            let e = due_elapsed(d, f).unwrap();
            assert!(r.max_frames(d, e) >= f && (e == 0 || r.max_frames(d, e - 1) < f), "due time of {f} at {d}");
            let w = window_frames(d, f, 60);
            assert_eq!(w, r.max_frames(d, e + 60).max(f));
            assert!(window_frames(d, f, 0) >= f && window_frames(d, f, 10) <= w);
        }
        // At equilibrium one minute of the auction is one difficulty halving's worth: about +17 %.
        let w = window_frames(D, 2000, 60);
        assert!((2300..=2400).contains(&w), "{w}");
        // At the cap the window can't grow further.
        assert_eq!(window_frames(1, 36_000, 60), 36_000);
        assert_eq!(due_elapsed(1, 36_001), None);
    }

    #[test]
    fn fork_choice_is_longer_and_heavier() {
        let s = |len, weight| Side { len, weight, tainted: false };
        assert!(prefer_alt(s(3, 20), s(4, 21), None));
        assert!(!prefer_alt(s(3, 20), s(4, 20), None), "longer but not heavier");
        assert!(!prefer_alt(s(3, 20), s(3, 40), None), "heavier but not longer");
        assert!(!prefer_alt(s(3, 20), s(3, 20), None), "a tie keeps ours");
        assert!(prefer_alt(s(18, 200), s(18, 201), Some(18)), "healing: deep and heavier");
        assert!(!prefer_alt(s(17, 200), s(19, 199), Some(18)));
        assert!(!prefer_alt(s(18, 200), s(17, 300), Some(18)), "both sides must be deep");
        assert!(prefer_alt(s(18, 200), s(17, 300), Some(17)));
        // v0.3's penalty: a tainted chain needs a lead of 2.
        let t = |len, weight| Side { len, weight, tainted: true };
        assert!(!prefer_alt(s(3, 20), t(4, 30), None));
        assert!(prefer_alt(s(3, 20), t(5, 30), None));
        assert!(prefer_alt(t(4, 20), s(4, 21), None), "a clean chain beats a tainted one of equal length");
    }
}
