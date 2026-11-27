//! [`Consensus`] from sm64pow: the rules are called, never copied (`pow/sm64pow` is the single source;
//! the daemon calls the same functions through its C ABI). Feature `consensus`; it links the game crate
//! but runs no emulation.
use sm64pow::records::{self, RecordFault};

use crate::block::Hash;
use crate::verify::{Consensus, Level, Parent1Facts, RecordsFault, Side};

/// sm64pow's active rules (`rules::active()`, `features::active()`).
pub struct Sm64Consensus;

impl Consensus for Sm64Consensus {
    fn level(&self) -> Level {
        Level::Full
    }
    fn launch_timestamp(&self) -> u64 {
        sm64pow::rules::active().launch_timestamp()
    }
    fn next_difficulty(&self, timestamps: &[u64], cumulative: &[u128]) -> Option<u64> {
        Some(sm64pow::rules::active().next_difficulty(timestamps, cumulative))
    }
    fn max_frames(&self, difficulty: u64, elapsed: u64) -> Option<u32> {
        Some(sm64pow::rules::active().max_frames(difficulty, elapsed))
    }
    fn check_signature(&self, seed: &Hash, nonce: &[u8], proof_id: &Hash, frames: u32, sig: &[u8; 64]) -> Result<Option<Hash>, String> {
        sm64pow::sign::check(seed, nonce, proof_id, frames, sig).map(Some).map_err(|e| e.to_string())
    }
    /// `stagec::count_records` of the daemon's path, but keeping the fault apart: a witness that does
    /// not match the block's commitment is the serving node's tampering, an invalid record is the block's.
    fn counted_records(&self, field: &[u8], nonce: &[u8], p: &Parent1Facts) -> Result<u32, RecordsFault> {
        let feats = sm64pow::features::active();
        if !feats.proof_weight || !feats.included_proofs || p.height == 0 {
            return Ok(0);
        }
        let Some(fields) = records::nonce_fields(nonce) else { return Ok(0) };
        let committed = fields.root.is_some();
        if field.is_empty() {
            return if committed { Err(RecordsFault::BadWitness("the block commits proofs but the node sent none".into())) } else { Ok(0) };
        }
        let recs = records::decode(field).map_err(|e| RecordsFault::BadWitness(format!("the proofs field does not parse: {e}")))?;
        let parent = sm64pow::included::Parent1 { height: p.height, prev_id: p.prev_id, prev_is_genesis: p.prev_is_genesis, nonce: p.nonce, proof: p.proof, difficulty: p.difficulty };
        let winner = parent.winner().map_err(RecordsFault::Invalid)?;
        match records::check_opts(&recs, fields.root.as_ref(), &winner, None, &records::PROPOSED) {
            Ok((_, counted)) => Ok(counted as u32),
            Err(RecordFault::BadWitness(w)) => Err(RecordsFault::BadWitness(w)),
            Err(RecordFault::Invalid(w)) => Err(RecordsFault::Invalid(w)),
        }
    }
    fn prefer_alt(&self, main: Side, alt: Side) -> bool {
        let s = |x: Side| records::Side { len: x.len, weight: x.weight, tainted: x.tainted };
        records::prefer_alt(s(main), s(alt), None)
    }
}
