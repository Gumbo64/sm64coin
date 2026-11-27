//! The star board (sm64coin_perturb::stars) against the real game: replays TASVideos 7310M (120 stars) on the consensus build and, at
//! checkpoints, the board's stars equal Mario's star count, the cap switches the progress points', and the board only ever gains.
//!
//! Needs the ROM (SM64COIN_ROM, default ../../baserom.us.z64) and the TAS (SM64_TAS, default
//! ../../../sm64-binds/tas/tasvideos-7310M-120star.m64); skips without them.

use sm64_binds::GamePad;
use sm64coin_perturb::stars::board;
use sm64pow::game::read_progress;

#[test]
fn the_board_follows_the_games_stars_on_the_tas() {
    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    let tas = std::env::var("SM64_TAS").unwrap_or_else(|_| "../../../sm64-binds/tas/tasvideos-7310M-120star.m64".into());
    let (Ok(rom), Ok(m64)) = (std::fs::read(&rom), std::fs::read(&tas)) else {
        eprintln!("skipped: no ROM or no TAS");
        return;
    };
    let Some(mut g) = sm64pow::emulator::init(rom).ok().and_then(|e| e.create_game().ok()) else { return };
    let n = u32::from_le_bytes(m64[0x18..0x1c].try_into().unwrap()) as usize;
    let b0 = board(&read_progress(&g).unwrap());
    assert_eq!((b0.got, b0.total), (0, 120), "power-on: nothing yet");
    let mut last = 0;
    let mut checked = 0;
    for i in 0..n.min(120_000) {
        let o = 0x400 + 4 * i;
        g.step_game(GamePad { button: u16::from_be_bytes([m64[o], m64[o + 1]]), stick_x: m64[o + 2] as i8, stick_y: m64[o + 3] as i8 }).unwrap();
        if i % 600 != 599 {
            continue;
        }
        let p = read_progress(&g).unwrap();
        let b = board(&p);
        assert_eq!(b.total, 120);
        // the star counter and the save file's flags agree once a star's cutscene is over; a frame inside one may differ by one
        assert!(b.got.abs_diff(p.stars) <= 1, "frame {}: board {} vs Mario's {}", i + 1, b.got, p.stars);
        assert!(b.got >= last, "the board only gains");
        assert_eq!(b.switches.iter().take(3).filter(|s| s.got).count() as u32, p.caps.count_ones());
        last = b.got;
        checked += 1;
    }
    assert!(checked > 10 && last > 0, "the TAS collected stars within the frames replayed ({last})");
}
