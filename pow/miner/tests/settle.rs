//! `settle` on the end state of the live v0.3 chain's block 3 (hash 7c0f9230...,
//! 3 stars; the parent of block 4): the star-dance exit into the castle. With
//! some perturbation seeds the pokes move the save menu's cursor to "save and
//! quit", A confirms it, and the game then waits in a level change for Start;
//! settle only pressed A, never saw Mario stand still, ran to the frame cap and
//! the miner exited ("settle: Mario never stood still after the parent's end
//! state"). About 1.5% of seeds at difficulty 5 (those below were found by scan).
use sm64coin_miner::rh::settle;
use sm64pow::proof::Start;
use sm64pow::{Seed, params};
use std::io::Read;

fn seed(i: u64) -> Seed {
    let mut b = [0u8; 32];
    for (k, x) in b.iter_mut().enumerate() {
        *x = ((i * 2654435761 + k as u64 * 40503).wrapping_mul(0x9E3779B97F4A7C15) >> 56) as u8;
    }
    Seed(b)
}

#[test]
fn settles_from_block_3_even_when_the_menu_quits() {
    if sm64pow::init_default().is_err() {
        eprintln!("no ROM; skipped");
        return;
    }
    let gz = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v03_block3.state.gz")).unwrap();
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut bytes).unwrap();
    let state = sm64pow::sm64_binds::SaveState::from_bytes(bytes).unwrap();
    let p = params(5);
    // 0, 1: the usual path (~334 frames); 95, 156, 166: stuck in the quit warp.
    for i in [0u64, 1, 95, 156, 166] {
        let go = || {
            let (mut game, _) = sm64pow::chain::mining_start(Start::Parent, Some(&state), &seed(i), &p).unwrap();
            let mut played = vec![];
            let st = settle(&mut game, &mut played, 4714).unwrap_or_else(|e| panic!("seed {i}: {e}"));
            assert!(st.controllable());
            played.len()
        };
        let n = go();
        assert!(n < 1000, "seed {i} settled after {n} frames");
        assert_eq!(n, go(), "deterministic");
    }
}
