//! Included proofs, the node's side (v0.4 stage B, layer 5): judging a block's `sm64_proofs`
//! field against its parent block h (docs/V0.4-DESIGN.md section 2.4), and the replay seam the
//! proof pool ([`crate::pool`]) shares with it.
//!
//! Everything that needs the game goes through a [`Judge`], so the rules around it (root first,
//! then structure, then the replays) are testable with a stand-in; [`Real`] is the replay.
//!
//! Normalizing: a record's proof may be carried in any encoding (the leaf only commits the
//! input hash). Nodes store and relay the smallest STATELESS one (codecs 0-2: no game needed to
//! decode, same leaf, same root). A state-coded proof (codecs 3-7) is decoded by the replay; the
//! pool gets its inputs from that replay and normalizes it too ([`Real::replay_one`]), but a
//! block's field is only normalized where the encoding is stateless (a state-coded record is
//! kept as it is: getting its inputs would cost a second replay of every such record).
//!
//! Read next: pool.rs.

use crate::chain::{BlockVerdict, CommittedVerdict, Parent, RecordsVerdict, natural_start, played_committed_block, rebuild_committed_block, verify_records};
use crate::commit;
use crate::proof::{Proof, Start};
use crate::records::{self, Facts, Record, RecordFault, Winner};
use crate::seed::Seed;
use anyhow::Result;

/// One record's run, judged.
#[derive(Debug, Clone, PartialEq)]
pub enum Judged {
    /// A valid run; its proof in the smallest stateless encoding when it could be made.
    Valid(Option<Vec<u8>>),
    /// The committed run is invalid.
    Invalid(String),
    /// The encoding doesn't decode to the committed inputs.
    BadWitness(String),
    NeedParent,
}

/// What needs the game: the natural start (from the parent's cached end state) and replays.
pub trait Judge: Sync {
    /// The start every proof of the height must have (None: the parent's end state isn't cached).
    fn start(&self, parent: &Parent) -> Option<Start>;
    /// All records' runs from `parent`, on `threads` threads (a block's records).
    fn replay_all(&self, parent: Parent, facts: &[Facts], proofs: &[&[u8]], threads: usize) -> Result<RecordsVerdict>;
    /// One record's run (the pool's offer).
    fn replay_one(&self, parent: Parent, facts: &Facts, proof: &[u8]) -> Result<Judged>;
}

/// The real replay.
pub struct Real;

/// The smallest stateless encoding of a committed proof (same header: same leaf, same block id).
/// None: it is state-coded (or doesn't decode): keep it as it is.
pub fn smallest_stateless(blob: &[u8]) -> Option<Vec<u8>> {
    match commit::open(blob, crate::rules::active().max_frames_any(), false) {
        Ok(commit::Opened::Run(_, pr)) => {
            let best = commit::commit(&pr);
            Some(if best.len() < blob.len() { best } else { blob.to_vec() })
        }
        _ => None,
    }
}

fn is_state_coded(blob: &[u8]) -> bool {
    commit::split(blob).ok().and_then(|(_, enc)| enc.first().copied()).is_some_and(|c| crate::statecodec::state_codec(c).is_some())
}

impl Judge for Real {
    fn start(&self, parent: &Parent) -> Option<Start> {
        natural_start(parent)
    }

    fn replay_all(&self, parent: Parent, facts: &[Facts], proofs: &[&[u8]], threads: usize) -> Result<RecordsVerdict> {
        verify_records(parent, facts, proofs, threads)
    }

    fn replay_one(&self, parent: Parent, f: &Facts, blob: &[u8]) -> Result<Judged> {
        let seed = Seed(f.seed);
        let id = f.leaf(&[0; 64]);
        let verdict = |v: CommittedVerdict| match v {
            CommittedVerdict::Run(BlockVerdict::Valid { .. }) => None,
            CommittedVerdict::Run(BlockVerdict::Invalid(why)) => Some(Judged::Invalid(why)),
            CommittedVerdict::Run(BlockVerdict::NeedParent) => Some(Judged::NeedParent),
            CommittedVerdict::BadEncoding(why) => Some(Judged::BadWitness(why)),
        };
        if is_state_coded(blob) && crate::statecodec::state_codec_enabled() {
            // The replay decodes it: take the played inputs, commit them in the smallest stateless
            // encoding, and run THAT too (a second replay, but it is what a block's field will
            // carry: its cache entry makes the including block's replay a hit).
            let pl = match played_committed_block(&id, parent, &seed, blob)? {
                Ok(pl) => pl,
                Err(v) => return Ok(verdict(v).unwrap_or(Judged::Invalid("no run".into()))),
            };
            let pr = Proof { frames: f.run.frames, start: f.run.start, inputs: pl.wanted() };
            if commit::proof_hash(&pr) != f.run.input_hash {
                return Ok(Judged::BadWitness("the played inputs do not hash to the committed input hash".into()));
            }
            let norm = commit::commit(&pr);
            return Ok(match verdict(rebuild_committed_block(&id, parent, &seed, &norm)?) {
                None => Judged::Valid(Some(norm)),
                Some(j) => j,
            });
        }
        Ok(match verdict(rebuild_committed_block(&id, parent, &seed, blob)?) {
            None => Judged::Valid(smallest_stateless(blob)),
            Some(j) => j,
        })
    }
}

/// Block h, the including block's parent: everything the records of block h+1 are judged against.
#[derive(Clone, Copy, Debug)]
pub struct Parent1<'a> {
    pub height: u64,
    /// B_{h-1}'s id.
    pub prev_id: [u8; 32],
    pub prev_is_genesis: bool,
    /// Block h's coinbase extra nonce (its key).
    pub nonce: &'a [u8],
    /// Block h's committed proof (frames, input hash).
    pub proof: &'a [u8],
    /// Block h's retarget difficulty.
    pub difficulty: u64,
}

impl Parent1<'_> {
    pub fn parent(&self) -> Parent {
        if self.prev_is_genesis { Parent::Genesis } else { Parent::Block { id: self.prev_id, gap_seconds: 0 } }
    }

    /// The facts of block h the records are judged against (Err: block h has no key or header).
    pub fn winner(&self) -> Result<Winner, String> {
        let key = crate::sign::miner_key(self.nonce).ok_or("block h commits no miner key")?;
        let run = commit::header(self.proof).map_err(|e| format!("block h's proof header: {e}"))?;
        Ok(Winner { height: self.height, prev_id: self.prev_id, key, frames: run.frames, difficulty: self.difficulty, input_hash: run.input_hash })
    }
}

/// What a block's records came to.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockRecords {
    /// Valid: how many count for weight (0 while `proof_weight` is off) and the field with every
    /// stateless proof in its smallest encoding (the same leaves and root; empty for no records).
    Valid { counted: u32, normalized: Vec<u8> },
    Invalid(String),
    /// Leaves don't give the root, a proof doesn't decode to its committed hash, unparseable records.
    BadWitness(String),
    /// B_{h-1}'s end state isn't cached.
    NeedParent,
}

/// Judges the `sm64_proofs` `field` of the block whose coinbase extra nonce is `nonce`
/// (docs/V0.4-DESIGN.md section 2.4) against its parent block h. `field` empty and no root: valid
/// at once, no state needed. With `included_proofs` off a block must carry neither.
pub fn verify_block_records(field: &[u8], nonce: &[u8], h: &Parent1, threads: usize, judge: &dyn Judge) -> Result<BlockRecords> {
    let feats = crate::features::active();
    // A malformed TLV area: nothing to read a root from. With the switch on that is the block's
    // fault (the nonce is committed); with it off v0.3 nonces are all there is.
    let fields = records::nonce_fields(nonce);
    if fields.is_none() && feats.included_proofs {
        return Ok(BlockRecords::Invalid("the coinbase extra nonce is malformed".into()));
    }
    let root = fields.and_then(|f| f.root);
    let committed = root.unwrap_or([0; 32]);
    if !feats.included_proofs {
        return Ok(if field.is_empty() && committed == [0; 32] { BlockRecords::Valid { counted: 0, normalized: vec![] } } else { BlockRecords::Invalid("included proofs are not active".into()) });
    }
    if field.is_empty() && committed == [0; 32] {
        return Ok(BlockRecords::Valid { counted: 0, normalized: vec![] });
    }
    let recs = if field.is_empty() {
        vec![]
    } else {
        match records::decode(field) {
            Ok(r) => r,
            Err(e) => return Ok(BlockRecords::BadWitness(format!("the proofs field does not parse: {e}"))),
        }
    };
    if recs.is_empty() && committed == [0; 32] {
        return Ok(BlockRecords::Valid { counted: 0, normalized: vec![] });
    }
    // Authenticity before anything that needs state: the leaves must give the committed root
    // (records::check does this too, but it wants the start first).
    let mut leaves = Vec::with_capacity(recs.len());
    for r in &recs {
        match r.leaf() {
            Ok(l) => leaves.push(l),
            Err(e) => return Ok(BlockRecords::BadWitness(format!("record does not parse: {e}"))),
        }
    }
    if records::root(&leaves) != committed {
        return Ok(BlockRecords::BadWitness("the records do not give the committed root".into()));
    }
    let winner = match h.winner() {
        Ok(w) => w,
        Err(e) => anyhow::bail!(e),
    };
    let parent = h.parent();
    let start = match judge.start(&parent) {
        Some(s) => s,
        None => return Ok(BlockRecords::NeedParent),
    };
    let (facts, counted) = match records::check(&recs, root.as_ref(), &winner, start, &records::PROPOSED) {
        Ok(x) => x,
        Err(RecordFault::BadWitness(w)) => return Ok(BlockRecords::BadWitness(w)),
        Err(RecordFault::Invalid(w)) => return Ok(BlockRecords::Invalid(w)),
    };
    let proofs: Vec<&[u8]> = recs.iter().map(|r| r.proof.as_slice()).collect();
    match judge.replay_all(parent, &facts, &proofs, threads)? {
        RecordsVerdict::Valid => {}
        RecordsVerdict::Invalid(i, why) => return Ok(BlockRecords::Invalid(format!("proof {i}: {why}"))),
        RecordsVerdict::BadWitness(i, why) => return Ok(BlockRecords::BadWitness(format!("proof {i}: {why}"))),
        RecordsVerdict::NeedParent => return Ok(BlockRecords::NeedParent),
    }
    Ok(BlockRecords::Valid { counted: counted as u32, normalized: normalize_field(&recs) })
}

/// The field with every stateless proof in its smallest encoding (state-coded ones kept).
pub fn normalize_field(recs: &[Record]) -> Vec<u8> {
    if recs.is_empty() {
        return vec![];
    }
    let normalized: Vec<Record> = recs.iter().map(|r| Record { proof: smallest_stateless(&r.proof).unwrap_or_else(|| r.proof.clone()), ..r.clone() }).collect();
    records::encode(&normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::tests::{PREV, D, record, run, set, winner};
    use crate::sign::Keypair;

    struct Fake {
        start: Option<Start>,
        verdict: fn() -> RecordsVerdict,
    }
    impl Judge for Fake {
        fn start(&self, _: &Parent) -> Option<Start> {
            self.start
        }
        fn replay_all(&self, _: Parent, _: &[Facts], _: &[&[u8]], _: usize) -> Result<RecordsVerdict> {
            Ok((self.verdict)())
        }
        fn replay_one(&self, _: Parent, _: &Facts, _: &[u8]) -> Result<Judged> {
            unreachable!()
        }
    }
    const OK: Fake = Fake { start: Some(Start::Parent), verdict: || RecordsVerdict::Valid };

    fn block_h() -> (Vec<u8>, Vec<u8>) {
        let (k, w) = winner();
        let pr = Proof { frames: w.frames, start: Start::Parent, inputs: vec![sm64_binds::GamePad::new(1, 0, 0); w.frames as usize - 10] };
        (crate::sign::key_nonce(&k.public).to_vec(), commit::commit_with(&crate::proof::RunLength, &pr))
    }

    fn judge(field: &[u8], root: Option<[u8; 32]>, j: &Fake) -> BlockRecords {
        let (hn, hp) = block_h();
        let h = Parent1 { height: 5, prev_id: PREV, prev_is_genesis: false, nonce: &hn, proof: &hp, difficulty: D };
        let k = Keypair::from_entropy(&[99; 32]).unwrap();
        let nonce = records::template_nonce(&k.public, None, root.as_ref());
        verify_block_records(field, &nonce, &h, 2, j).unwrap()
    }

    #[test]
    fn no_field_and_no_root_is_valid_without_any_state() {
        let none = Fake { start: None, verdict: || panic!("no replay") };
        assert_eq!(judge(&[], None, &none), BlockRecords::Valid { counted: 0, normalized: vec![] });
        assert_eq!(judge(&[], Some([0; 32]), &none), BlockRecords::Valid { counted: 0, normalized: vec![] }, "a zero root is none");
        assert_eq!(judge(&[0], None, &none), BlockRecords::Valid { counted: 0, normalized: vec![] }, "varint(0)");
    }

    #[test]
    fn valid_records_are_counted_and_normalized() {
        let (rs, root) = set(6);
        let field = records::encode(&rs);
        match judge(&field, Some(root), &OK) {
            BlockRecords::Valid { counted, normalized } => {
                assert_eq!(counted, 6);
                let n = records::decode(&normalized).unwrap();
                assert!(normalized.len() <= field.len());
                for (a, b) in rs.iter().zip(&n) {
                    assert_eq!((a.leaf().unwrap(), &a.sig), (b.leaf().unwrap(), &b.sig));
                }
                assert_eq!(records::root(&n.iter().map(|r| r.leaf().unwrap()).collect::<Vec<_>>()), root, "same root");
            }
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn faults_are_classified() {
        let (rs, root) = set(4);
        let field = records::encode(&rs);
        // the state first: -2, but only after the witness checks that need none
        let nostate = Fake { start: None, verdict: || panic!() };
        assert_eq!(judge(&field, Some(root), &nostate), BlockRecords::NeedParent);
        assert!(matches!(judge(&field, Some([5; 32]), &nostate), BlockRecords::BadWitness(_)), "wrong root: a witness fault whatever the state");
        assert!(matches!(judge(&field, None, &OK), BlockRecords::BadWitness(_)), "records but no root TLV");
        assert!(matches!(judge(&[], Some(root), &OK), BlockRecords::BadWitness(_)), "a root but no records");
        assert!(matches!(judge(&field[..field.len() - 3], Some(root), &OK), BlockRecords::BadWitness(_)), "unparseable");
        // a rule broken by the leaves: invalid (here: out of order)
        let mut swapped = rs.clone();
        swapped.swap(0, 1);
        let leaves: Vec<_> = swapped.iter().map(|r| r.leaf().unwrap()).collect();
        assert!(matches!(judge(&records::encode(&swapped), Some(records::root(&leaves)), &OK), BlockRecords::Invalid(_)));
        // a duplicate of the winner's key
        let own = record(&winner().0, 5, &PREV, &run(2100, 9), 0).0;
        let leaves = [own.leaf().unwrap()];
        assert!(matches!(judge(&records::encode(&[own]), Some(records::root(&leaves)), &OK), BlockRecords::Invalid(_)));
        // the replays' verdicts
        let inv = Fake { start: Some(Start::Parent), verdict: || RecordsVerdict::Invalid(2, "no progress".into()) };
        assert!(matches!(judge(&field, Some(root), &inv), BlockRecords::Invalid(m) if m.contains("proof 2")));
        let bad = Fake { start: Some(Start::Parent), verdict: || RecordsVerdict::BadWitness(1, "x".into()) };
        assert!(matches!(judge(&field, Some(root), &bad), BlockRecords::BadWitness(_)));
        let need = Fake { start: Some(Start::Parent), verdict: || RecordsVerdict::NeedParent };
        assert_eq!(judge(&field, Some(root), &need), BlockRecords::NeedParent);
        // the wrong natural start
        let po = Fake { start: Some(Start::PowerOn), verdict: || RecordsVerdict::Valid };
        assert!(matches!(judge(&field, Some(root), &po), BlockRecords::Invalid(_)));
    }
}
