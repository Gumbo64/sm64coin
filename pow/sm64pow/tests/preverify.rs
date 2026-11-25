//! The parallel pre-pass (chain::preverify): the runs it replays are found by
//! verify_block without another replay, with the same verdicts.

use sm64pow::Seed;
use sm64pow::chain::{BatchBlock, BlockVerdict, CommittedVerdict, Parent, preverify, replays, verify_committed_block as verify_block};

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

fn fixture() -> (Seed, Vec<u8>, u32) {
    let j: serde_json::Value = serde_json::from_str(include_str!("fixtures/power_on_star.json")).unwrap();
    let seed = Seed::from_hex(j["seed_hash"].as_str().unwrap()).unwrap();
    let h = j["proof_hex"].as_str().unwrap();
    let blob = (0..h.len() / 2).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()).collect();
    (seed, blob, j["frames"].as_u64().unwrap() as u32)
}

#[test]
fn preverified_runs_are_not_replayed_again() {
    if !init() {
        return;
    }
    let (seed, blob, frames) = fixture();
    // (v0.4: committed blobs, commit.rs)
    let blob = sm64pow::commit::commit_layout(&blob, &sm64pow::commit::proof_hash(&sm64pow::proof::decode_proof(&blob, 36_000).unwrap())).unwrap();
    // Two independent power-on runs (two "cycles"): both are chains of their own.
    let batch = vec![
        BatchBlock { id: [41; 32], parent: Parent::Genesis, seed, blob: blob.clone() },
        BatchBlock { id: [42; 32], parent: Parent::Genesis, seed, blob: blob.clone() },
    ];
    // The same run twice (same parent, seed and proof): one replay, the other is the same run.
    let n = preverify(&batch, 2);
    assert!(n >= 1 && n <= 2, "{n}");
    let before = replays();
    let v = verify_block(&[41; 32], Parent::Genesis, &seed, 1, &blob).unwrap();
    assert!(matches!(v, CommittedVerdict::Run(BlockVerdict::Valid { frames: f, .. }) if f == frames), "{v:?}");
    assert_eq!(replays(), before, "verify_block replayed a run the pre-pass had replayed");
    // The other copy is found too, without a replay.
    assert!(matches!(verify_block(&[42; 32], Parent::Genesis, &seed, 1 << 20, &blob).unwrap(), CommittedVerdict::Run(BlockVerdict::Valid { .. })));
    assert_eq!(replays(), before);
    // A block the pre-pass never saw (another seed) is replayed as before.
    let mut other = seed;
    other.0[0] ^= 1;
    let _ = verify_block(&[43; 32], Parent::Genesis, &other, 1, &blob).unwrap();
    assert!(replays() > before);
}
