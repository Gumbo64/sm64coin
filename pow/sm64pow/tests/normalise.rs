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

/// The replay-free canonicaliser (src/exact.rs) on the same blocks: per block the exact rules'
/// run is replay-checked and sealed; sizes and times are printed next to the replay-guided
/// normaliser's (NORMALISE_BLOCKS=n, default 3; NORMALISE_REPLAY=1 also times the replay-guided
/// setting and the exact+replay combination).
#[test]
fn exact_canonicaliser_on_chain_blocks() {
    use sm64pow::normalise::{Method, StickPick, exact_pads, seal_proof_exact_report};
    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    let Ok(b) = std::fs::read(&rom) else {
        eprintln!("skipping: no ROM at {rom} (set SM64COIN_ROM)");
        return;
    };
    let _ = sm64pow::init(b);
    let n: usize = std::env::var("NORMALISE_BLOCKS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let both = std::env::var("NORMALISE_REPLAY").is_ok();
    let mut ts = std::collections::HashMap::new();
    let mut cycle: Vec<sm64pow::sm64_binds::GamePad> = vec![];
    let (mut orig_total, mut exact_total, mut frames_total) = (0usize, 0usize, 0usize);
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
        let played = played_block(&id, parent, &seed, &blob).unwrap().unwrap();
        if start == Start::PowerOn {
            cycle.clear();
        }
        let parent_state = if start == Start::Parent { Some(sm64pow::statecodec::state_after(&cycle).unwrap()) } else { None };
        let p = sm64pow::params(difficulty);
        let t0 = std::time::Instant::now();
        let pick = if std::env::var("NORMALISE_PICK").as_deref() == Ok("rep") { StickPick::Rep } else { StickPick::Model };
        let (pads, st) = exact_pads(start, parent_state.as_ref(), &seed, &p, &played, pick).unwrap_or_else(|e| panic!("block {}: the exact check failed: {e:#}", j["height"]));
        let te = t0.elapsed().as_secs_f64();
        assert_eq!(pads.len(), played.len());
        let method = if both { Method::ExactThenReplay } else { Method::Exact };
        let t1 = std::time::Instant::now();
        let (sealed, rep) = seal_proof_exact_report(start, parent_state.as_ref(), &seed, &p, &played, method, pick).unwrap();
        eprintln!(
            "block {}: {frames} frames, original {} B, exact {:?} B ({:.1} %), exact+replay {:?}, sealed {} B ({:.1} %); exact canonicaliser {te:.2} s, plain seal {:.2} s, seal total {:.2} s; sticks changed {} (neutral {}), bits cleared {} set {}, pinned {}",
            j["height"], rep.original, rep.exact, 100.0 * rep.exact.unwrap_or(rep.original) as f64 / rep.original as f64, rep.exact_then_replay, sealed.len(),
            100.0 * sealed.len() as f64 / rep.original as f64, rep.plain_secs, t1.elapsed().as_secs_f64(), st.sticks_changed, st.sticks_neutral, st.button_bits_cleared, st.button_bits_set, st.pinned
        );
        assert!(sealed.len() <= rep.original);
        orig_total += rep.original;
        exact_total += rep.exact.unwrap_or(rep.original);
        frames_total += frames as usize;
        cycle.extend(played);
    }
    eprintln!("total: {frames_total} frames, original {orig_total} B, exact {exact_total} B ({:.1} %)", 100.0 * exact_total as f64 / orig_total as f64);
}

/// Old (replay-guided) and new (exact) canonicaliser on the fixture blocks: each sealed proof must
/// verify with the node's verifier (`verify_any`, the same verdict as the original run's proof:
/// frames, progress, cycle end) and decode to the run's frame count. (Both sealers only return a
/// candidate whose verifier replay ends in the original's memory and perturbation state, check_sealed.) NORMALISE_BLOCKS=n (default 3); NORMALISE_OLD=0 skips the slow replay-guided one.
#[test]
fn exact_and_replay_guided_proofs_verify_alike() {
    use sm64pow::normalise::{Method, StickPick, seal_proof_exact_report, seal_proof_normalised_report};
    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    let Ok(b) = std::fs::read(&rom) else {
        eprintln!("skipping: no ROM at {rom} (set SM64COIN_ROM)");
        return;
    };
    let _ = sm64pow::init(b);
    let n: usize = std::env::var("NORMALISE_BLOCKS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let old = std::env::var("NORMALISE_OLD").as_deref() != Ok("0");
    let mut ts = std::collections::HashMap::new();
    let mut cycle: Vec<sm64pow::sm64_binds::GamePad> = vec![];
    let (mut o, mut e, mut r, mut fr) = (0usize, 0usize, 0usize, 0usize);
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
        let played = played_block(&id, parent, &seed, &blob).unwrap().unwrap();
        if start == Start::PowerOn {
            cycle.clear();
        }
        let ps = if start == Start::Parent { Some(sm64pow::statecodec::state_after(&cycle).unwrap()) } else { None };
        let p = sm64pow::params(difficulty);
        let mut other = id;
        other[0] ^= 0xff;
        let verdict = |blob: &[u8]| match sm64pow::chain::verify_any(&other, parent, &seed, difficulty, blob).unwrap() {
            sm64pow::chain::CommittedVerdict::Run(v @ BlockVerdict::Valid { .. }) => v,
            v => panic!("block {}: not valid: {v:?}", j["height"]),
        };
        let (exact, rep) = seal_proof_exact_report(start, ps.as_ref(), &seed, &p, &played, Method::Exact, StickPick::Model).unwrap();
        let mut cands = vec![("exact", exact.clone())];
        if old {
            cands.push(("replay-guided", seal_proof_normalised_report(start, ps.as_ref(), &seed, &p, &played).unwrap().0));
        }
        let (orig_pads, _) = (played.clone(), ());
        let base = verdict(&sm64pow::seal_proof_at(start, ps.as_ref(), &seed, &p, &orig_pads).unwrap());
        let mut sizes = vec![];
        for (name, c) in &cands {
            assert_eq!(verdict(c), base, "block {} {name}: verdict differs from the original run's", j["height"]);
            let pr = sm64pow::mining::decode_proof_any(c, ps.as_ref(), &seed, &p).unwrap();
            assert_eq!(pr.frames, frames);
            sizes.push((*name, c.len()));
        }
        eprintln!("block {}: {frames} frames, original {} B, {sizes:?}", j["height"], rep.original);
        o += rep.original;
        e += exact.len();
        r += cands.get(1).map_or(0, |c| c.1.len());
        fr += frames as usize;
        cycle.extend(played);
    }
    eprintln!("total {fr} frames: original {o} B, exact {e} B ({:.1} %), replay-guided {r} B ({:.1} %)", 100.0 * e as f64 / o as f64, 100.0 * r as f64 / o as f64);
}
