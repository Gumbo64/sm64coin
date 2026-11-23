//! The chain rules on a real run: the AutoMiner's first block (power-on to
//! the first star), then blocks on top of it.

use sm64pow::chain::{BlockVerdict, Parent, played_block, rebuild_block, verify_block};
use sm64pow::proof::{self, Proof, Start};
use sm64pow::sm64_binds::GamePad;
use sm64pow::{Seed, played_from_proof};

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

fn id(n: u8) -> [u8; 32] {
    [n; 32]
}

/// A short run that makes no progress, starting at `start`.
fn idle(start: Start) -> Vec<u8> {
    proof::encode_proof(&Proof { frames: 60, start, inputs: vec![GamePad::default(); 60] })
}

#[test]
fn a_chain_of_blocks() {
    if !init() {
        return;
    }
    let (seed, blob, frames) = fixture();
    // Block 1: power-on on top of the genesis block.
    let v = verify_block(&id(1), Parent::Genesis, &seed, 1, &blob).unwrap();
    let BlockVerdict::Valid { frames: f, progress, ends_cycle } = v else { panic!("{v:?}") };
    assert_eq!(f, frames);
    assert_eq!((progress.stars, progress.keys, ends_cycle), (1, 0, false));
    // Bookkeeping: which star (Bob-omb Battlefield, star 6: Behind Chain Chomp's Gate).
    let ev = sm64pow::chain::cached(&id(1)).unwrap().event.map(|e| e.name());
    assert_eq!(ev.as_deref(), Some("c01s6"));
    // Too tight a time limit (a high difficulty) rejects it.
    assert!(matches!(verify_block(&id(9), Parent::Genesis, &seed, 1 << 20, &blob).unwrap(), BlockVerdict::Invalid(_)));
    // The first block can't claim a parent start.
    assert!(matches!(verify_block(&id(9), Parent::Genesis, &seed, 1, &idle(Start::Parent)).unwrap(), BlockVerdict::Invalid(_)));

    // Block 2 continues from block 1's end state: an idle run makes no progress.
    let b1 = Parent::Block { id: id(1), gap_seconds: 100 };
    match verify_block(&id(2), b1, &seed, 1, &idle(Start::Parent)).unwrap() {
        BlockVerdict::Invalid(why) => assert!(why.contains("no progress"), "{why}"),
        v => panic!("{v:?}"),
    }
    // Unknown parent: the caller must replay it first.
    let unknown = Parent::Block { id: id(77), gap_seconds: 100 };
    assert_eq!(verify_block(&id(2), unknown, &seed, 1, &idle(Start::Parent)).unwrap(), BlockVerdict::NeedParent);

    // Starting over from power-on (a restart): not until the restart delay,
    // which grows with the progress thrown away (block 1 has one star)...
    let delay = sm64pow::rules().restart_delay(&progress).unwrap();
    match verify_block(&id(3), b1, &seed, 1, &blob).unwrap() {
        BlockVerdict::Invalid(why) => assert!(why.contains("restart"), "{why}"),
        v => panic!("{v:?}"),
    }
    // ...and then judged against the limit that many seconds earlier.
    let later = Parent::Block { id: id(1), gap_seconds: delay };
    assert!(matches!(verify_block(&id(3), later, &seed, 1, &blob).unwrap(), BlockVerdict::Valid { .. }));
    let hard = 1 << 20; // the run is far over this difficulty's limit at 0 s
    assert!(matches!(verify_block(&id(3), later, &seed, hard, &blob).unwrap(), BlockVerdict::Invalid(_)));
    // The delay needs the parent's progress: an unknown parent is replayed first.
    let unknown_later = Parent::Block { id: id(77), gap_seconds: delay };
    assert_eq!(verify_block(&id(3), unknown_later, &seed, 1, &blob).unwrap(), BlockVerdict::NeedParent);
    // Rebuilding an accepted restart needs no parent state.
    assert!(matches!(rebuild_block(&id(3), unknown_later, &seed, &blob).unwrap(), BlockVerdict::Valid { .. }));

    // Played pads (for replays) agree with the standalone replay.
    let played = played_block(&id(1), Parent::Genesis, &seed, &blob).unwrap().unwrap();
    let p = sm64pow::params(1);
    let pr = proof::decode_proof(&blob, 100_000).unwrap();
    assert_eq!(played, played_from_proof(&seed, &p, &pr).unwrap());
    assert_eq!(played.len() as u32, frames);
    // Rebuilding (no time limit) works even at a difficulty that would reject it.
    assert!(matches!(rebuild_block(&id(4), Parent::Genesis, &seed, &blob).unwrap(), BlockVerdict::Valid { .. }));
    // The standalone verifier treats a run as a power-on block.
    assert_eq!(sm64pow::verify(&seed, 1, &blob).unwrap(), sm64pow::Verdict::Valid { frames });
}

#[test]
fn sealing_a_run_from_a_parent_state() {
    if !init() {
        return;
    }
    let (seed, blob, _) = fixture();
    let p = sm64pow::params(1);
    // The parent's end state, as a miner gets it (replaying the cycle's
    // played pads, unperturbed, from power-on).
    let played = played_block(&id(11), Parent::Genesis, &seed, &blob).unwrap().unwrap();
    let mut g = sm64pow::generator().unwrap().create_game().unwrap();
    for pad in &played {
        g.step_game(*pad).unwrap();
    }
    let parent_state = g.save_state();
    assert_eq!(sm64pow::game::read_progress(&g).unwrap().stars, 1);
    // A miner on top: a (non-winning) run from that state round-trips through
    // its proof.
    let seed2 = Seed([42; 32]);
    let (mut mg, before) = sm64pow::chain::mining_start(Start::Parent, Some(&parent_state), &seed2, &p).unwrap();
    assert_eq!(before.stars, 1);
    let mut pads = vec![];
    for i in 0..300 {
        pads.push(mg.step(GamePad::new(if i % 20 < 10 { 0x8000 } else { 0 }, 0, 50)).unwrap().0);
    }
    let pr = sm64pow::proof_from_played_at(Start::Parent, Some(&parent_state), &seed2, &p, &pads).unwrap();
    assert_eq!(pr.start, Start::Parent);
    assert_eq!(sm64pow::played_from_proof_at(Some(&parent_state), &seed2, &p, &pr).unwrap(), pads);
}
