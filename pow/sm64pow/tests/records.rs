//! v0.4 included proofs: replaying records' runs (docs/V0.4-DESIGN.md section 2.4 rule 9) on
//! real runs. Two power-on runs from the genesis block's child position (v0.2 mainnet block 1,
//! state codec; the AutoMiner's xz block), each under its own seed, as the proofs of one height.
//! The structural rules are unit-tested in records.rs.

use sm64pow::Seed;
use sm64pow::chain::{Parent, RecordsVerdict, natural_start, played_wanted_block, verify_records};
use sm64pow::commit;
use sm64pow::proof::{self, Proof, RunLength, Start};
use sm64pow::records::{Facts, Payout};

fn init() -> bool {
    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    match std::fs::read(&rom) {
        Ok(b) => {
            let _ = sm64pow::init(b);
            true
        }
        Err(_) => {
            eprintln!("skipping: no ROM at {rom} (set SM64COIN_ROM)");
            false
        }
    }
}

fn unhex(h: &str) -> Vec<u8> {
    (0..h.len() / 2).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn facts(seed: &Seed, blob: &[u8]) -> Facts {
    Facts { seed: seed.0, key: [1; 32], payout: Payout { subaddress: false, spend: [2; 32], view: [3; 32] }, prev_id: [0; 32], height: 1, run: commit::header(blob).unwrap() }
}

/// (seed, committed state-coded blob, its run) of v0.2 mainnet block 1, and the same for the xz block.
fn runs() -> [(Seed, Vec<u8>, Proof); 2] {
    let j: serde_json::Value = serde_json::from_str(include_str!("fixtures/mainnet_v02.jsonl").lines().next().unwrap()).unwrap();
    let seed = Seed::from_hex(j["seed"].as_str().unwrap()).unwrap();
    let blob = unhex(j["proof"].as_str().unwrap());
    let pl = played_wanted_block(&[0x41; 32], Parent::Genesis, &seed, &blob).unwrap().unwrap();
    let (frames, start, _, _) = proof::split_blob(&blob).unwrap();
    let a = Proof { frames, start, inputs: pl.wanted() };
    let a_blob = commit::commit_layout(&blob, &commit::proof_hash(&a)).unwrap();
    let j: serde_json::Value = serde_json::from_str(include_str!("fixtures/power_on_star.json")).unwrap();
    let seed_b = Seed::from_hex(j["seed_hash"].as_str().unwrap()).unwrap();
    let xz = unhex(j["proof_hex"].as_str().unwrap());
    let b = proof::decode_proof(&xz, 36_000).unwrap();
    let b_blob = commit::commit_layout(&xz, &commit::proof_hash(&b)).unwrap();
    [(seed, a_blob, a), (seed_b, b_blob, b)]
}

#[test]
fn records_replay_and_faults_are_classified() {
    if !init() {
        return;
    }
    assert_eq!(natural_start(&Parent::Genesis), Some(Start::PowerOn));
    assert_eq!(natural_start(&Parent::Block { id: [0xee; 32], gap_seconds: 0 }), None, "unknown parent: rebuild first");
    let [(sa, a, pa), (sb, b, pb)] = runs();
    let fs = [facts(&sa, &a), facts(&sb, &b)];
    assert_eq!(verify_records(Parent::Genesis, &fs, &[&a, &b], 2).unwrap(), RecordsVerdict::Valid);
    // Any encoding of the same runs: still valid (cache hits for the stateless one).
    let a_rle = commit::commit_with(&RunLength, &pa);
    assert_eq!(verify_records(Parent::Genesis, &fs, &[&a_rle, &b], 1).unwrap(), RecordsVerdict::Valid);
    // A stateless run under another seed: it decodes to its committed inputs, but the pokes
    // don't fit: the committed run is invalid.
    let swapped = [facts(&sa, &a), facts(&sa, &b)];
    assert!(matches!(verify_records(Parent::Genesis, &swapped, &[&a, &b], 2).unwrap(), RecordsVerdict::Invalid(1, _)));
    // A state-coded body under another seed decodes (in other game states) to other inputs: that
    // says nothing about the committed run, so it is a bad witness, not an invalid block.
    let swapped = [facts(&sb, &a), facts(&sb, &b)];
    assert!(matches!(verify_records(Parent::Genesis, &swapped, &[&a, &b], 2).unwrap(), RecordsVerdict::BadWitness(0, _)));
    // A body that doesn't decode to the committed inputs: a bad witness (record 1).
    let mut bad = b.clone();
    let n = bad.len();
    bad[n - 20] ^= 0xff;
    assert!(matches!(verify_records(Parent::Genesis, &fs, &[&a, &bad], 2).unwrap(), RecordsVerdict::BadWitness(1, _)));
    // The short run (one frame less, its own committed hash): invalid.
    let short = Proof { frames: pb.frames - 1, start: Start::PowerOn, inputs: pb.inputs[..pb.inputs.len() - 1].to_vec() };
    let s = commit::commit_with(&RunLength, &short);
    assert!(matches!(verify_records(Parent::Genesis, &[facts(&sa, &a), facts(&sb, &s)], &[&a, &s], 2).unwrap(), RecordsVerdict::Invalid(1, _)));
}
