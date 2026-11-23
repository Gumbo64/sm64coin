//! The search's hooked game plays exactly as the verifier's mining game:
//! same pads, same pokes, same memory, through forks and save/load.

use rand::{Rng, SeedableRng, rngs::SmallRng};
use sm64_binds::GamePad;
use sm64coin_miner::hook::{hooked, hooked_save, mining_save};
use sm64pow::{Seed, new_mining_game, params};

#[test]
fn hooked_game_steps_as_the_verifier() {
    if sm64pow::init_default().is_err() {
        eprintln!("no ROM; skipped");
        return;
    }
    let seed = Seed([7u8; 32]);
    let mut mine = new_mining_game(&seed, &params(1)).unwrap();
    let mut rng = SmallRng::seed_from_u64(1);
    let mut pad = || GamePad { button: rng.random::<u16>() & 0xb000, stick_x: rng.random(), stick_y: rng.random() };
    for _ in 0..600 {
        mine.step_game(GamePad::default()).unwrap();
    }
    let mut search = hooked(&mine).unwrap();
    for f in 0..1500 {
        let p = pad();
        let (a, _) = mine.step(p).unwrap();
        let b = search.step(p).unwrap();
        assert_eq!((a.button, a.stick_x, a.stick_y), (b.button, b.stick_x, b.stick_y), "pad at frame {f}");
    }
    assert!(mine.game.save_state() == search.game.save_state(), "memory after 1500 frames");
    // Through a fork, a save and a load.
    let saved = search.save_state();
    let mut fork = search.fork().unwrap();
    for _ in 0..300 {
        fork.step_game(pad()).unwrap();
    }
    fork.load_state(&saved).unwrap();
    let back = mining_save(&fork.save_state()).unwrap();
    let mut replay = new_mining_game(&seed, &params(1)).unwrap();
    replay.load_state(&back).unwrap();
    search.load_state(&hooked_save(&back)).unwrap();
    for _ in 0..600 {
        let p = pad();
        replay.step_game(p).unwrap();
        search.step_game(p).unwrap();
    }
    assert!(replay.game.save_state() == search.game.save_state(), "memory after fork, save and load");
}
