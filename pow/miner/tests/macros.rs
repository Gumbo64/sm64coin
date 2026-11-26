//! The boot and settle macros (src/macros.rs) over many perturbation seeds: Mario is controllable
//! at the first frame the game allows, the star is saved and nothing gets stuck, also under pokes far
//! harsher than the rules'. The legacy fixed schedules (rh::boot_legacy / settle_legacy) are checked
//! the same way, including the save-and-quit recovery.
//!
//! Needs the ROM (SM64COIN_ROM or ./baserom.us.z64); skipped without it.
use obrrt_core::{GamePad, read_state};
use sm64coin_miner::{macros, rh};
use sm64pow::game::{Perturber, PokeConfig};
use sm64pow::proof::Start;
use sm64pow::{SM64MiningGame, Seed, params};
use std::io::Read;

fn seed(i: u64) -> Seed {
    let mut b = [0u8; 32];
    for (k, x) in b.iter_mut().enumerate() {
        *x = ((i * 2654435761 + k as u64 * 40503).wrapping_mul(0x9E3779B97F4A7C15) >> 56) as u8;
    }
    Seed(b)
}

fn rom() -> bool {
    if sm64pow::init_default().is_err() {
        eprintln!("no ROM; skipped");
        return false;
    }
    true
}

/// `bursts` poked bursts of 2 per 100 ticks (the rules: 5), the pad stream and timing from the seed.
fn harsher(game: &mut SM64MiningGame, i: u64, bursts: u32) {
    let c = PokeConfig { window_length: 100, random_amount: bursts, burst_length: 2, a_prob: 0.5, b_prob: 0.5, z_prob: 0.2, timing_mix: 100, pad_mix: 100 };
    game.pert = Perturber::new(&seed(i).0, &c);
}

fn parent() -> sm64pow::sm64_binds::SaveState {
    // The end of sm64pow's power_on_star fixture: BoB star 6 (the chomp star), 1 star (examples/mkparent.rs).
    let gz = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v04_chomp.state.gz")).unwrap();
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut bytes).unwrap();
    sm64pow::sm64_binds::SaveState::from_bytes(bytes).unwrap()
}

type Macro = fn(&mut SM64MiningGame, &mut Vec<GamePad>, usize) -> anyhow::Result<obrrt_core::St>;

fn from_power_on(f: Macro, i: u64, bursts: Option<u32>) -> (usize, obrrt_core::St) {
    let (mut game, _) = sm64pow::chain::mining_start(Start::PowerOn, None, &seed(i), &params(5)).unwrap();
    if let Some(b) = bursts {
        harsher(&mut game, i, b);
    }
    let mut played = vec![];
    let st = f(&mut game, &mut played, 4000).unwrap_or_else(|e| panic!("seed {i}: {e:#}"));
    (played.len(), st)
}

fn from_parent(f: Macro, state: &sm64pow::sm64_binds::SaveState, i: u64, bursts: Option<u32>) -> (usize, obrrt_core::St) {
    let (mut game, _) = sm64pow::chain::mining_start(Start::Parent, Some(state), &seed(i), &params(5)).unwrap();
    if let Some(b) = bursts {
        harsher(&mut game, i, b);
    }
    let mut played = vec![];
    let st = f(&mut game, &mut played, 4000).unwrap_or_else(|e| panic!("seed {i}: {e:#}"));
    (played.len(), st)
}

/// The boot: the controllable frame after the 1478-frame intro cutscene, 1646-1649 frames in under the
/// rules' pokes (the old schedule: 1680 fixed frames, then Mario must stand still: 1690-1816).
#[test]
fn boot_is_controllable_at_the_first_frame_the_game_allows() {
    if !rom() {
        return;
    }
    let mut worst = 0;
    for i in 0..300 {
        let (n, st) = from_power_on(macros::boot, i, None);
        assert!(st.controllable() && st.level == 16 && st.stars == 0, "seed {i}: {st:?}");
        assert!((1646..=1660).contains(&n), "seed {i}: handed over at {n}");
        worst = worst.max(n);
    }
    // Deterministic: a function of the state and the seed's pokes.
    assert_eq!(from_power_on(macros::boot, 7, None).0, from_power_on(macros::boot, 7, None).0);
    eprintln!("boot: worst {worst}");
}

#[test]
fn boot_survives_pokes_far_harsher_than_the_rules() {
    if !rom() {
        return;
    }
    // 50 bursts of 2 per 100 ticks (the rules: 5): the file select, the dialog and the idle all get poked.
    for i in 0..150 {
        let (n, st) = from_power_on(macros::boot, i, Some(50));
        assert!(st.controllable() && st.level == 16 && st.stars == 0, "seed {i}: {st:?}");
        assert!(n < 1700, "seed {i}: handed over at {n}");
    }
}

#[test]
fn legacy_boot_is_still_there_and_slower() {
    if !rom() {
        return;
    }
    for i in 0..20 {
        let (old, st) = from_power_on(rh::boot_legacy, i, None);
        assert!(st.controllable());
        let (new, _) = from_power_on(macros::boot, i, None);
        assert!(old >= 1690 && new + 40 <= old, "seed {i}: legacy {old}, new {new}");
    }
    // The flag routes rh::boot to the legacy schedule.
    rh::set_legacy_macros(true);
    let (via_flag, _) = from_power_on(rh::boot, 3, None);
    rh::set_legacy_macros(false);
    assert_eq!(via_flag, from_power_on(rh::boot_legacy, 3, None).0);
    assert_eq!(from_power_on(rh::boot, 3, None).0, from_power_on(macros::boot, 3, None).0);
}

/// The settle: the star dance, the save menu (A takes effect once its timer is past 110), the milestone
/// dialog, then control, ~355 frames; the star is saved (menu option 1) however the cursor was poked.
#[test]
fn settle_saves_the_star_and_hands_over_at_the_first_controllable_frame() {
    if !rom() {
        return;
    }
    let state = parent();
    for i in 0..300 {
        let (n, st) = from_parent(macros::settle, &state, i, None);
        assert!(st.controllable() && st.has_star(1, 5) && st.stars == 1, "seed {i}: {st:?}");
        assert!((355..=375).contains(&n), "seed {i}: handed over at {n}");
    }
}

#[test]
fn settle_survives_pokes_far_harsher_than_the_rules() {
    if !rom() {
        return;
    }
    let state = parent();
    // 50 bursts per 100 ticks: the legacy schedule picks "save and quit" for 14% of the seeds
    // (below: 27, 29, 37 and others).
    for i in 0..300 {
        let (n, st) = from_parent(macros::settle, &state, i, Some(50));
        assert!(st.controllable() && st.has_star(1, 5) && st.stars == 1, "seed {i}: {st:?}");
        assert!(n < 380, "seed {i}: handed over at {n}");
    }
}

/// The legacy settle after the menu picks "save and quit": the game waits at its title screen for Start.
#[test]
fn legacy_settle_still_recovers_from_save_and_quit() {
    if !rom() {
        return;
    }
    let state = parent();
    for i in [27u64, 29, 37] {
        let (n, st) = from_parent(rh::settle_legacy, &state, i, Some(50));
        assert!(st.controllable(), "seed {i}");
        assert!(n > 380 && n < 1000, "seed {i}: legacy settle took {n} (the quit path)");
        let (new, st2) = from_parent(macros::settle, &state, i, Some(50));
        assert!(new < n && st2.has_star(1, 5), "seed {i}: new {new}");
    }
    // Under the rules' pokes it saves the star on the usual path too.
    let (n, st) = from_parent(rh::settle_legacy, &state, 0, None);
    assert!(st.controllable() && st.has_star(1, 5) && n < 400);
    let _ = read_state;
}

/// Adversarial pokes by hand around the save menu: every pair of pads (a poke burst of two frames) from a
/// small menu-hostile set, at every frame from before the options show to after the menu is gone. The
/// macro must still hand over a controllable Mario with the star saved (menu option 1 or 2; 3 would lose it:
/// a stick move from line 1 to line 3 needs two scrolls, which a burst of two frames can't do), and
/// must not take long. Pokes play instead of the macro's pad, the rest of the run is the macro's.
#[test]
fn settle_survives_every_burst_around_the_save_menu() {
    if !rom() {
        return;
    }
    let state = parent();
    let a = obrrt_core::buttons::A_BUTTON;
    let set = [GamePad::new(0, 0, 0), GamePad::new(a, 0, 0), GamePad::new(0, 0, -80), GamePad::new(a, 0, -80), GamePad::new(0, 0, 80), GamePad::new(a, 0, 80)];
    let play = |pokes: &[(usize, GamePad)]| -> (usize, obrrt_core::St, usize, usize) {
        let (mut mg, _) = sm64pow::chain::mining_start(Start::Parent, Some(&state), &seed(0), &params(5)).unwrap();
        // No random pokes: only the hand-made ones.
        mg.pert = Perturber::new(&seed(0).0, &PokeConfig { window_length: 100, random_amount: 0, burst_length: 2, a_prob: 0.5, b_prob: 0.5, z_prob: 0.2, timing_mix: 0, pad_mix: 0 });
        let mut played: Vec<GamePad> = vec![];
        let (mut menu_from, mut menu_to) = (usize::MAX, 0);
        let mut warp_wait = 0;
        let mut st = read_state(&mg, 0, false);
        while !st.controllable() {
            assert!(played.len() < 1500, "stuck with {pokes:?}");
            let prev = played.last().copied().unwrap_or_default();
            let mut p = macros::pad(&st, prev, warp_wait);
            if let Some((_, q)) = pokes.iter().find(|(t, _)| *t == played.len()) {
                p = *q;
            }
            if st.in_save_menu() && st.dialog_state == 1 {
                menu_from = menu_from.min(played.len());
                menu_to = played.len();
            }
            mg.game.step_game(p).unwrap();
            played.push(p);
            st = read_state(&mg, played.len() as u32, false);
            warp_wait = if st.play_mode != 0 && !st.modal() && !st.controllable() { warp_wait + 1 } else { 0 };
        }
        (played.len(), st, menu_from, menu_to)
    };
    let (n0, _, from, to) = play(&[]);
    assert!((355..=360).contains(&n0), "{n0}");
    let mut worst = 0;
    let mut quits = 0;
    for t in from.saturating_sub(3)..to + 4 {
        for &p1 in &set {
            for &p2 in &set {
                let (n, st, _, _) = play(&[(t, p1), (t + 1, p2)]);
                assert!(st.has_star(1, 5) && st.stars == 1, "burst at {t}: {p1:?} {p2:?}: {st:?}");
                worst = worst.max(n);
                quits += (n > 380) as usize;
            }
        }
    }
    eprintln!("menu {from}..{to}: worst {worst}, {quits} bursts made it save and quit");
    assert!(worst < 520);
}

/// What the macros play is an ordinary run: the verifier replays it (the perturbation conforms on every
/// frame, so the only complaint is that no star was made), from power-on and from a parent's end state.
#[test]
fn macro_runs_replay_in_the_verifier() {
    if !rom() {
        return;
    }
    let p = params(5);
    for i in 0..3 {
        let (mut game, _) = sm64pow::chain::mining_start(Start::PowerOn, None, &seed(i), &p).unwrap();
        let mut played = vec![];
        macros::boot(&mut game, &mut played, 4000).unwrap();
        for k in 0..200 {
            played.push(game.step(GamePad::new(if k % 3 == 0 { 0x8000 } else { 0 }, 0, 0)).unwrap().0);
        }
        match sm64pow::verify_pads(&seed(i), &p, &played).unwrap() {
            sm64pow::Verdict::Invalid(why) => assert!(why.contains("no progress"), "seed {i}: {why}"),
            v => panic!("{v:?}"),
        }
    }
    let state = parent();
    for i in 0..3 {
        let (mut game, _) = sm64pow::chain::mining_start(Start::Parent, Some(&state), &seed(i), &p).unwrap();
        let mut played = vec![];
        macros::settle(&mut game, &mut played, 4000).unwrap();
        for _ in 0..100 {
            played.push(game.step(GamePad::default()).unwrap().0);
        }
        let pr = sm64pow::proof_from_played_at(Start::Parent, Some(&state), &seed(i), &p, &played).unwrap();
        assert_eq!(sm64pow::played_from_proof_at(Some(&state), &seed(i), &p, &pr).unwrap(), played, "seed {i}");
    }
}
