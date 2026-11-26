//! The miner's input normaliser (src/normalise.rs; feature `normalise`) on real chain blocks: the
//! v0.2 mainnet's first blocks (fixtures/mainnet_v02.jsonl). For each block the run as played is
//! normalised with both settings and sealed; the sealed proof must be valid (success first on the
//! last frame), have the same frame count, reach the original's end state, and be no larger than
//! the original's. NORMALISE_BLOCKS=n (default 3) sets how many blocks; prints the sizes.
#![cfg(feature = "normalise")]

use sm64pow::Seed;
use sm64pow::chain::{BlockVerdict, Parent, played_block, verify_block};
use sm64pow::proof::Start;
use sm64pow::normalise::{AGGRESSIVE, CONSERVATIVE, normalise_played, seal_proof_normalised_report};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

#[test]
fn normalised_blocks_replay_identically_and_are_smaller() {
    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    let Ok(b) = std::fs::read(&rom) else {
        eprintln!("skipping: no ROM at {rom} (set SM64COIN_ROM)");
        return;
    };
    let _ = sm64pow::init(b);
    let n: usize = std::env::var("NORMALISE_BLOCKS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let mut ts = std::collections::HashMap::new();
    let mut cycle: Vec<sm64pow::sm64_binds::GamePad> = vec![];
    let (mut orig_total, mut sealed_total, mut frames_total) = (0usize, 0usize, 0usize);
    for l in include_str!("fixtures/mainnet_v02.jsonl").lines().take(n) {
        let j: serde_json::Value = serde_json::from_str(l).unwrap();
        let id: [u8; 32] = hex(j["id"].as_str().unwrap()).try_into().unwrap();
        let prev: [u8; 32] = hex(j["prev"].as_str().unwrap()).try_into().unwrap();
        let t = j["ts"].as_u64().unwrap();
        let parent = match ts.get(&prev) {
            None => Parent::Genesis,
            Some(pt) => Parent::Block { id: prev, gap_seconds: t - pt },
        };
        ts.insert(id, t);
        let seed = Seed::from_hex(j["seed"].as_str().unwrap()).unwrap();
        let difficulty = j["difficulty"].as_u64().unwrap();
        let blob = hex(j["proof"].as_str().unwrap());
        let (frames, start, _, _) = sm64pow::proof::split_blob(&blob).unwrap();
        assert!(matches!(verify_block(&id, parent, &seed, difficulty, &blob).unwrap(), BlockVerdict::Valid { .. }));
        let played = played_block(&id, parent, &seed, &blob).unwrap().unwrap();
        assert_eq!(played.len(), frames as usize);
        if start == Start::PowerOn {
            cycle.clear();
        }
        let parent_state = if start == Start::Parent { Some(sm64pow::statecodec::state_after(&cycle).unwrap()) } else { None };
        let p = sm64pow::params(difficulty);
        let t0 = std::time::Instant::now();
        let (sealed, rep) = seal_proof_normalised_report(start, parent_state.as_ref(), &seed, &p, &played).unwrap();
        eprintln!("block {}: {frames} frames, original {} B, conservative {:?}, aggressive {:?}, sealed {} B ({:.1} %), {:.1} s", j["height"], rep.original, rep.tried[0], rep.tried[1], sealed.len(), 100.0 * sealed.len() as f64 / rep.original as f64, t0.elapsed().as_secs_f64());
        assert!(sealed.len() <= rep.original);
        // The sealed proof is a valid block's proof for the same template: the node's check.
        let again = sm64pow::mining::decode_proof_any(&sealed, parent_state.as_ref(), &seed, &p).unwrap();
        assert_eq!(again.frames, frames);
        let mut other = id;
        other[0] ^= 0xff; // another block id: the same template, the normalised run
        // v0.4: the sealed proof is a committed blob (the fixture blocks are v0.2/v0.3 layout).
        match sm64pow::chain::verify_any(&other, parent, &seed, difficulty, &sealed).unwrap() {
            sm64pow::chain::CommittedVerdict::Run(BlockVerdict::Valid { frames: f, .. }) => assert_eq!(f, frames),
            v => panic!("block {}: the normalised proof is not a valid block: {v:?}", j["height"]),
        }
        for s in [&CONSERVATIVE, &AGGRESSIVE] {
            let pads = normalise_played(start, parent_state.as_ref(), &seed, &p, &played, s).unwrap();
            assert_eq!(pads.len(), played.len());
        }
        orig_total += rep.original;
        sealed_total += sealed.len();
        frames_total += frames as usize;
        cycle.extend(played);
    }
    eprintln!("total: {frames_total} frames, original {orig_total} B, sealed {sealed_total} B ({:.1} % of the original)", 100.0 * sealed_total as f64 / orig_total as f64);
}
