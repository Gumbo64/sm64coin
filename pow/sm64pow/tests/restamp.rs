//! Red team: re-stamped copies of a block (its own process: it counts replays).

use sm64pow::chain::{BlockVerdict, Parent, verify_block};
use sm64pow::Seed;

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
    let blob = (0..j["proof_hex"].as_str().unwrap().len() / 2)
        .map(|i| u8::from_str_radix(&j["proof_hex"].as_str().unwrap()[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    (seed, blob, j["frames"].as_u64().unwrap() as u32)
}

/// Red team F1: the seed excludes the timestamp, so anyone can re-stamp a
/// published block into any number of new block ids carrying the same run.
/// Verifying such variants must not replay the run again, and must not evict
/// other blocks' end states from the cache.
#[test]
fn restamped_variants_are_not_replayed_and_do_not_thrash_the_cache() {
    if !init() {
        return;
    }
    let (seed, blob, frames) = fixture();
    let base = [0xa0; 32];
    assert!(matches!(verify_block(&base, Parent::Genesis, &seed, 1, &blob).unwrap(), BlockVerdict::Valid { .. }));
    let before = sm64pow::chain::replays();
    // 200 variants (distinct ids, as re-stamping gives), more than the 24 cached states.
    for k in 0..200u32 {
        let mut vid = [0xb0; 32];
        vid[..4].copy_from_slice(&k.to_le_bytes());
        let v = verify_block(&vid, Parent::Genesis, &seed, 1, &blob).unwrap();
        assert!(matches!(v, BlockVerdict::Valid { frames: f, .. } if f == frames), "{v:?}");
        assert!(sm64pow::chain::cached(&vid).is_some());
    }
    assert_eq!(sm64pow::chain::replays(), before, "variants of a verified run are not replayed");
    assert!(sm64pow::chain::cached(&base).is_some(), "variants do not evict other states");
    // The timestamp-dependent rules still apply to every variant: the power-on start (a
    // restart: only after the restart delay, which depends on the parent's progress)...
    let b1 = Parent::Block { id: base, gap_seconds: 100 };
    match verify_block(&[0xc1; 32], b1, &seed, 1, &blob).unwrap() {
        BlockVerdict::Invalid(why) => assert!(why.contains("restart") || why.contains("over"), "{why}"),
        v => panic!("{v:?}"),
    }
    let delay = sm64pow::rules().restart_delay(&sm64pow::chain::cached(&base).unwrap().progress).unwrap();
    let o = sm64pow::rules::STAR_CYCLE.start_halvings * sm64pow::rules::STAR_CYCLE.decay_halving;
    let later = Parent::Block { id: base, gap_seconds: delay + o };
    assert!(matches!(verify_block(&[0xc2; 32], later, &seed, 1, &blob).unwrap(), BlockVerdict::Valid { .. }));
    // ...and the time limit (a hard difficulty's limit then is far below the run).
    assert!(matches!(verify_block(&[0xc3; 32], later, &seed, 1 << 20, &blob).unwrap(), BlockVerdict::Invalid(_)));
    assert_eq!(sm64pow::chain::replays(), before);
}
