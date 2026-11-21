//! A MiningEnv run is what the consensus verifier accepts, including across
//! reseed boundaries. Needs the ROM (`$SM64COIN_ROM` or ../../baserom.us.z64).
use rand::{Rng, SeedableRng, rngs::SmallRng};
use sm64coin_sdk::*;

fn rom() {
    let p = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| format!("{}/../../baserom.us.z64", env!("CARGO_MANIFEST_DIR")));
    init_rom(&p).expect("ROM");
}

fn random_pad(r: &mut SmallRng) -> GamePad {
    GamePad::new(if r.random_bool(0.3) { buttons::A_BUTTON } else { 0 }, r.random_range(-80..=80), r.random_range(-80..=80))
}

/// 1000 random frames cross three reseed boundaries (300, 600, 900). The
/// verifier must find every recorded pad conforming (so the only complaint is
/// that no star was reached), and it must be the same at every difficulty.
#[test]
fn random_run_conforms_across_reseeds() {
    rom();
    for difficulty in [1u64, 64] {
        let task = Task::practice(11, difficulty);
        let mut env = MiningEnv::new(&task).unwrap();
        let mut r = SmallRng::seed_from_u64(3);
        let mut poked = 0;
        for _ in 0..1000 {
            let w = random_pad(&mut r);
            poked += (env.step(w).unwrap() != w) as usize;
        }
        assert!(poked > 50, "perturbation should poke: {poked}");
        match sm64pow::verify_pads(&task.seed, &task.params, env.pads()).unwrap() {
            Verdict::Invalid(why) => assert_eq!(why, "success condition never met"),
            v => panic!("{v:?}"),
        }
    }
}

/// The perturbation is what makes the run: skipping a reseed (what a bot that
/// forgot the contract would do) is rejected at the first reseed boundary.
#[test]
fn forgetting_to_reseed_is_caught() {
    rom();
    let task = Task::practice(5, 1);
    let mut g = sm64pow::new_mining_game(&task.seed, &task.params).unwrap();
    let mut r = SmallRng::seed_from_u64(9);
    let mut pads = vec![];
    for _ in 0..700 {
        let p = g.rng_pad(random_pad(&mut r)).unwrap(); // no reseed
        g.step_game(p).unwrap();
        pads.push(p);
    }
    match sm64pow::verify_pads(&task.seed, &task.params, &pads).unwrap() {
        Verdict::Invalid(why) => assert!(why.contains("perturbation forces"), "{why}"),
        v => panic!("{v:?}"),
    }
}

/// fork / save / restore follow the same line as the original.
#[test]
fn fork_and_restore_are_exact() {
    rom();
    let task = Task::practice(2, 1);
    let mut env = MiningEnv::new(&task).unwrap();
    let mut r = SmallRng::seed_from_u64(1);
    for _ in 0..290 {
        env.step(random_pad(&mut r)).unwrap();
    }
    let snap = env.save();
    let mut fork = env.fork().unwrap();
    let futures: Vec<GamePad> = (0..40).map(|_| random_pad(&mut r)).collect(); // crosses frame 300
    for w in &futures {
        env.step(*w).unwrap();
        fork.step(*w).unwrap();
    }
    assert_eq!(env.pads(), fork.pads());
    assert_eq!(env.state().unwrap(), fork.state().unwrap());
    env.restore(&snap).unwrap();
    assert_eq!(env.frame(), 290);
    for w in &futures {
        env.step(*w).unwrap();
    }
    assert_eq!(env.pads(), fork.pads(), "replay after restore reproduces the same played pads");
    assert_eq!(env.state().unwrap(), fork.state().unwrap());
}

/// A real winning run (found by the built-in beam search; see
/// examples/make_fixture.rs), replayed through MiningEnv, ends on the success
/// frame and its proof is valid, in every codec.
#[test]
fn winning_fixture_verifies() {
    rom();
    let path = format!("{}/fixtures/win_seed1_d1.json", env!("CARGO_MANIFEST_DIR"));
    let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).unwrap();
    let seed = Seed::from_hex(j["seed_hash"].as_str().unwrap()).unwrap();
    let raw = hex::decode(j["pads_raw_hex"].as_str().unwrap()).unwrap();
    let wanted: Vec<GamePad> = raw.chunks_exact(4).map(proof::pad_from).collect();
    assert!(wanted.len() > 600, "fixture must cross reseed boundaries");
    let mut env = MiningEnv::new(&Task::new(seed, 1)).unwrap();
    for (i, w) in wanted.iter().enumerate() {
        assert!(!env.is_success(), "success before the last frame ({i})");
        assert_eq!(env.step(*w).unwrap(), *w, "frame {i}: fixture pads are already perturbed");
    }
    assert!(env.is_success());
    assert!(env.step(GamePad::default()).is_err(), "no frames after success");
    assert_eq!(env.verify().unwrap(), Verdict::Valid { frames: wanted.len() as u32 });
    for c in sm64pow::proof::CODECS {
        let blob = proof::encode_with(*c, env.pads());
        assert_eq!(verify(&seed, 1, &blob).unwrap(), Verdict::Valid { frames: wanted.len() as u32 });
    }
    // Truncated: not a win.
    assert!(matches!(verify(&seed, 1, &proof::encode_best(&wanted[..wanted.len() - 1])).unwrap(), Verdict::Invalid(_)));
}

#[test]
fn practice_report_and_task_params() {
    rom();
    struct Idle;
    impl Bot for Idle {
        fn play(&mut self, env: &mut MiningEnv, _stop: &dyn Fn() -> bool) -> anyhow::Result<()> {
            for _ in 0..350 {
                env.step(GamePad::default())?;
            }
            Ok(())
        }
    }
    let r = practice(&mut Idle, 4, 1, &|| false).unwrap();
    assert_eq!((r.frames, r.success, r.max_frames), (350, false, 18000));
    assert!(r.verdict.is_none());
}
