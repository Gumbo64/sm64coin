//! The SDK's boot / settle macros play exactly the pads the miner's do (sm64coin_miner::macros, the
//! automine and web miners' macros), from power-on and from a parent's end state. Needs the ROM.
use sm64coin_sdk::{MiningEnv, Task, macros};
use sm64pow::Seed;
use sm64pow::proof::Start;

fn rom() -> bool {
    if sm64coin_sdk::init_rom("").is_err() {
        eprintln!("no ROM; skipped");
        return false;
    }
    true
}

fn seed(i: u64) -> Seed {
    let mut b = [0u8; 32];
    for (k, x) in b.iter_mut().enumerate() {
        *x = ((i * 2654435761 + k as u64 * 40503).wrapping_mul(0x9E3779B97F4A7C15) >> 56) as u8;
    }
    Seed(b)
}

#[test]
fn boot_matches_the_miners() {
    if !rom() {
        return;
    }
    for i in 0..25 {
        let task = Task::new(seed(i), 5);
        let mut env = MiningEnv::new(&task).unwrap();
        let n = macros::boot(&mut env).unwrap();
        assert_eq!(n, env.frame());
        assert!((1646..=1660).contains(&n), "seed {i}: {n}");
        let (mut game, _) = sm64pow::chain::mining_start(Start::PowerOn, None, &seed(i), &sm64pow::params(5)).unwrap();
        let mut played = vec![];
        sm64coin_miner::macros::boot(&mut game, &mut played, 4000).unwrap();
        assert_eq!(env.pads(), &played[..], "seed {i}");
    }
}

#[test]
fn settle_matches_the_miners() {
    if !rom() {
        return;
    }
    let gz = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../miner/tests/fixtures/v04_chomp.state.gz")).unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&gz[..]), &mut bytes).unwrap();
    let state = sm64pow::sm64_binds::SaveState::from_bytes(bytes).unwrap();
    let progress = sm64pow::game::Progress { stars: 1, ..Default::default() };
    for i in 0..25 {
        let task = Task::after(state.clone(), progress, seed(i), 5);
        let mut env = MiningEnv::new(&task).unwrap();
        let n = macros::settle(&mut env).unwrap();
        assert!((355..=375).contains(&n), "seed {i}: {n}");
        let (mut game, _) = sm64pow::chain::mining_start(Start::Parent, Some(&state), &seed(i), &sm64pow::params(5)).unwrap();
        let mut played = vec![];
        sm64coin_miner::macros::settle(&mut game, &mut played, 4000).unwrap();
        assert_eq!(env.pads(), &played[..], "seed {i}");
    }
}
