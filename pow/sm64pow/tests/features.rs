//! Each v0.4 consensus feature on and off (features.rs; docs/V0.4-DESIGN.md section 10). The
//! switches are process-wide: one test, its own process, the cases in sequence.

use sm64pow::Seed;
use sm64pow::chain::{BlockVerdict, CommittedVerdict, Parent, verify_any};
use sm64pow::features::{self, Features};
use sm64pow::records::{self, PROPOSED_REWARD, Side};

fn unhex(h: &str) -> Vec<u8> {
    (0..h.len() / 2).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()).collect()
}

#[test]
fn each_feature_switches_on_its_own() {
    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    let have_rom = std::fs::read(&rom).map(|b| sm64pow::init(b).is_ok()).unwrap_or(false);
    let j: serde_json::Value = serde_json::from_str(include_str!("fixtures/power_on_star.json")).unwrap();
    let seed = Seed::from_hex(j["seed_hash"].as_str().unwrap()).unwrap();
    let old = unhex(j["proof_hex"].as_str().unwrap()); // v0.3 layout (xz)
    let pr = sm64pow::proof::decode_proof(&old, 36_000).unwrap();
    let new = sm64pow::commit::commit_layout(&old, &sm64pow::commit::proof_hash(&pr)).unwrap();
    let valid = |v: CommittedVerdict| matches!(v, CommittedVerdict::Run(BlockVerdict::Valid { .. }));
    let s = |len, weight| Side { len, weight, tainted: false };

    // Nothing of v0.4: v0.3's layout and id; no records; longest chain; no proof rewards.
    features::set(Features::NONE).unwrap();
    assert_eq!(sm64pow::proof_id(&old), sm64pow::proof_id::proof_id(&old));
    assert!(records::check(&[], None, &winner(), sm64pow::proof::Start::Parent, &records::PROPOSED).is_ok());
    assert!(records::prefer_alt(s(3, 100), s(4, 4), None), "longest chain: weight ignored");
    assert_eq!(records::proof_reward(1 << 40, 1, &PROPOSED_REWARD), 0);
    if have_rom {
        assert!(valid(verify_any(&[1; 32], Parent::Genesis, &seed, 1, &old).unwrap()), "v0.3 blob valid");
        assert!(!valid(verify_any(&[2; 32], Parent::Genesis, &seed, 1, &new).unwrap()), "a committed blob is not a v0.3 proof");
        let sealed = sm64pow::seal_proof(&seed, &sm64pow::params(1), &sm64pow::played_from_proof(&seed, &sm64pow::params(1), &pr).unwrap()).unwrap();
        assert_eq!(sm64pow::decode_proof_any(&sealed, None, &seed, &sm64pow::params(1)).unwrap(), pr, "sealing emits the v0.3 layout");
        assert!(sm64pow::commit::split(&sealed).map_or(true, |(h, _)| h.input_hash != sm64pow::commit::proof_hash(&pr)), "no committed hash in it");
    }

    // Stage A: committed inputs only.
    features::set(Features::STAGE_A).unwrap();
    assert_eq!(sm64pow::proof_id(&new), sm64pow::commit::proof_hash(&pr));
    assert_eq!(sm64pow::proof_id(&new), sm64pow::proof_id::proof_id(&old), "the v0.3 stateless id is the input hash");
    assert!(matches!(records::check(&[], Some(&[7; 32]), &winner(), sm64pow::proof::Start::Parent, &records::PROPOSED), Err(records::RecordFault::Invalid(_))), "no proofs root in stage A");
    assert!(records::prefer_alt(s(3, 100), s(4, 4), None));
    assert_eq!(records::proof_reward(1 << 40, 1, &PROPOSED_REWARD), 0);
    if have_rom {
        assert!(valid(verify_any(&[3; 32], Parent::Genesis, &seed, 1, &new).unwrap()), "committed blob valid");
        assert!(!valid(verify_any(&[4; 32], Parent::Genesis, &seed, 1, &old).unwrap()), "a v0.3 blob is not a committed proof");
    }

    // Stage B: proofs as data; still no weight, no rewards.
    features::set(Features::STAGE_B).unwrap();
    assert!(records::prefer_alt(s(3, 100), s(4, 4), None));
    assert_eq!(records::proof_reward(1 << 40, 1, &PROPOSED_REWARD), 0);

    // Stage C pieces, each alone on top of B.
    features::set(Features { proof_weight: true, ..Features::STAGE_B }).unwrap();
    assert!(!records::prefer_alt(s(3, 100), s(4, 4), None), "longer AND heavier");
    assert_eq!(records::proof_reward(1 << 40, 1, &PROPOSED_REWARD), 0);
    features::set(Features { proof_rewards: true, ..Features::STAGE_B }).unwrap();
    assert!(records::prefer_alt(s(3, 100), s(4, 4), None));
    assert_eq!(records::proof_reward(1 << 40, 1, &PROPOSED_REWARD), (1u64 << 40) / 128);

    assert!(features::set(Features { committed_inputs: false, ..Features::STAGE_B }).is_err(), "dependencies are enforced");
    features::set(Features::ALL).unwrap();
}

fn winner() -> records::Winner {
    records::Winner { height: 1, prev_id: [0; 32], key: [1; 32], frames: 2000, difficulty: 1 << 16, input_hash: [2; 32] }
}
