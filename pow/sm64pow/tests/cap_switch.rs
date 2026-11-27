//! When a cap switch counts (v0.5, star-cycle-70-caps): Progress::points() goes up on the frame after Mario first
//! stands on the switch (cap_switch_act_1 sets SAVE_FLAG_HAVE_*_CAP in gSaveBuffer as soon as gMarioObject->platform is
//! the switch, before its cutscene and dialog, and before any save to EEPROM), and one frame of contact at the switch's
//! edge is enough. Replays TASVideos 7310M (120 stars) unperturbed on the consensus build.
//!
//! Needs the ROM (SM64COIN_ROM, default ../../baserom.us.z64) and the TAS (SM64_TAS, default
//! ../../../sm64-binds/tas/tasvideos-7310M-120star.m64); skips without them.

use sm64_binds::{GamePad, SM64Game};
use sm64pow::game::{Event, event, read_progress};

// The consensus build's (us_web_headless, GAME_BUILD) linker map; struct Object field offsets (wasm32 = N64 layout).
const OBJECT_POOL: u32 = 0xb8ca90;
const OBJECT_SIZE: u32 = 0x260;
const MARIO_OBJECT: u32 = 0xb8c378;
const BHV_CAP_SWITCH: u32 = 0x7f72c0;
const DIALOG_ID: u32 = 0x7bd208;
const MARIO_STATE: u32 = 0xb0a010;
const OBJ_ACTIVE_FLAGS: u32 = 0x74;
const OBJ_BEHAVIOR: u32 = 0x20c;
const OBJ_PLATFORM: u32 = 0x214;
const OBJ_POS_X: u32 = 0xa0;
const OBJ_POS_Z: u32 = 0xa8;

fn u32_at(g: &SM64Game, a: u32) -> u32 {
    let mut b = [0u8; 4];
    g.read_memory(a, &mut b).unwrap();
    u32::from_le_bytes(b)
}

fn f32_at(g: &SM64Game, a: u32) -> f32 {
    f32::from_bits(u32_at(g, a))
}

fn cap_switch(g: &SM64Game) -> Option<u32> {
    (0..240).map(|k| OBJECT_POOL + k * OBJECT_SIZE).find(|&o| u32_at(g, o + OBJ_ACTIVE_FLAGS) & 1 != 0 && u32_at(g, o + OBJ_BEHAVIOR) == BHV_CAP_SWITCH)
}

fn on_switch(g: &SM64Game) -> bool {
    let mario = u32_at(g, MARIO_OBJECT);
    mario != 0 && cap_switch(g).is_some_and(|s| u32_at(g, mario + OBJ_PLATFORM) == s)
}

fn setup() -> Option<(SM64Game, Vec<GamePad>)> {
    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    let tas = std::env::var("SM64_TAS").unwrap_or_else(|_| "../../../sm64-binds/tas/tasvideos-7310M-120star.m64".into());
    let (Ok(rom), Ok(m64)) = (std::fs::read(&rom), std::fs::read(&tas)) else {
        eprintln!("skipped: no ROM or no TAS");
        return None;
    };
    let g = sm64pow::emulator::init(rom).ok()?.create_game().ok()?;
    let n = u32::from_le_bytes(m64[0x18..0x1c].try_into().unwrap()) as usize;
    let pads = (0..n).map(|i| {
        let o = 0x400 + 4 * i;
        GamePad { button: u16::from_be_bytes([m64[o], m64[o + 1]]), stick_x: m64[o + 2] as i8, stick_y: m64[o + 3] as i8 }
    });
    Some((g, pads.collect()))
}

#[test]
fn a_cap_switch_counts_the_frame_after_mario_lands_on_it() {
    let Some((mut g, pads)) = setup() else { return };
    let mut before = read_progress(&g).unwrap();
    let mut contact: Option<usize> = None;
    let mut seen = vec![];
    for (i, &pad) in pads.iter().enumerate().take(15_000) {
        g.step_game(pad).unwrap();
        let frames = i + 1;
        if on_switch(&g) && contact.is_none() {
            contact = Some(frames);
        }
        let now = read_progress(&g).unwrap();
        if now.caps != before.caps {
            assert_eq!(now.points(), before.points() + 1, "a cap switch is one progress point");
            assert_eq!(now.base_points(), before.base_points(), "but not one the restart delay prices");
            let c = contact.take().expect("the flag came with Mario on the switch");
            assert_eq!(frames, c + 1, "the flag (and the point) the frame after the first contact");
            assert_eq!(u32_at(&g, DIALOG_ID) as u16 as i16, -1, "before the switch's dialog");
            seen.push((event(&before, &now), frames));
        }
        if !on_switch(&g) && now.caps == before.caps {
            contact = None;
        }
        before = now;
    }
    // 7310M: the vanish cap switch in VCutM (it jumps straight off), then the wing cap switch flying into TotWC.
    assert_eq!(seen, vec![(Some(Event::Cap(3)), 2534), (Some(Event::Cap(1)), 14318)]);
    eprintln!("cap switches pressed at frames {seen:?}");
}

#[test]
fn one_frame_on_the_edge_of_the_switch_counts() {
    let Some((mut g, pads)) = setup() else { return };
    for &pad in &pads[..14_308] {
        g.step_game(pad).unwrap();
    }
    let start = read_progress(&g).unwrap();
    let sw = cap_switch(&g).expect("TotWC's switch is loaded");
    let (sx, sz) = (f32_at(&g, sw + OBJ_POS_X), f32_at(&g, sw + OBJ_POS_Z));
    // Flying in off the TAS's line (stick held up-right, no Z so no ground pound): Mario clips the switch's edge for one
    // frame and is off it the next.
    let mut on = vec![];
    for f in 1..=12 {
        g.step_game(GamePad { button: 0, stick_x: 32, stick_y: 32 }).unwrap();
        let d = (f32_at(&g, MARIO_STATE + 60) - sx).hypot(f32_at(&g, MARIO_STATE + 68) - sz);
        if on_switch(&g) {
            on.push((f, d));
        }
        if read_progress(&g).unwrap().points() > start.points() {
            assert_eq!(on.len(), 1, "one frame on the switch: {on:?}");
            assert!(on[0].1 > 150.0, "at its edge: {on:?}");
            assert_eq!(f, on[0].0 + 1);
            assert_eq!(event(&start, &read_progress(&g).unwrap()), Some(Event::Cap(1)));
            return;
        }
    }
    panic!("the edge contact did not press the switch: {on:?}");
}
