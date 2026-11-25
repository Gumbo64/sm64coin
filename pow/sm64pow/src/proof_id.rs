//! What a block id commits to for its proof.

use crate::rules::active as rules;
use crate::{proof, statecodec};
use sha3::{Digest, Keccak256};

/// What a block id commits to: Keccak-256 of the decoded proof (frame count,
/// start and inputs), so a proof can be re-encoded with another codec without
/// changing any block id. An undecodable blob gets a hash of its bytes behind
/// a marker (such a block fails verification anyway).
pub fn proof_id(blob: &[u8]) -> [u8; 32] {
    if statecodec::state_codec_enabled() {
        if let Ok((_, _, id, _)) = proof::split_blob(blob) {
            if statecodec::state_codec(id).is_some() {
                return statecodec::state_proof_id(blob);
            }
        }
    }
    let mut h = Keccak256::new();
    // No valid proof is longer than the easiest task allows; decoding no
    // further keeps a tiny compressed blob from costing megabytes here.
    match proof::decode_proof(blob, rules().max_frames_any() as usize) {
        Ok(pr) => {
            h.update(b"sm64run2");
            h.update(pr.frames.to_le_bytes());
            h.update([pr.start as u8]);
            for p in &pr.inputs {
                h.update(proof::pad_bytes(p));
            }
        }
        Err(_) => {
            h.update(b"sm64bad!");
            h.update(blob);
        }
    }
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sm64_binds::GamePad;

    #[test]
    fn proof_id_ignores_codec() {
        let pr = proof::Proof { frames: 50, start: proof::Start::Parent, inputs: vec![GamePad::new(0, 0, 0); 40] };
        let a = proof::encode_proof_with(&proof::Raw, &pr);
        let b = proof::encode_proof_with(&proof::RunLength, &pr);
        let c = proof::encode_proof_with(&proof::Xz, &pr);
        assert_ne!(a, b);
        assert_eq!(proof_id(&a), proof_id(&b));
        assert_eq!(proof_id(&a), proof_id(&c));
        assert_ne!(proof_id(&a), proof_id(&[0, 1, 2]));
        let other = proof::Proof { frames: 51, ..pr.clone() };
        assert_ne!(proof_id(&a), proof_id(&proof::encode_proof(&other)), "frame count is committed");
        let power_on = proof::Proof { start: proof::Start::PowerOn, ..pr.clone() };
        assert_ne!(proof_id(&a), proof_id(&proof::encode_proof(&power_on)), "start is committed");
    }
}
