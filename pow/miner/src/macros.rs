//! The miners' macros: the inputs played before the search starts, from power-on ([`boot`]) or from a
//! parent's end state ([`settle`]), up to the first frame Mario is controllable.
//!
//! State-reactive: every frame the pad is a function of the game's state (what menu or dialog is open,
//! where its cursor is, the last pad actually played) and of nothing else, never of the block's seed.
//! The perturbation replaces the whole pad on poked frames (A/B/Z and the stick from the seed, every other
//! button released), so a press that was meant for a frame may be lost and a poke may move a cursor; each
//! frame therefore looks at the state again and presses what moves it towards "continue".
//!
//! What the game needs (SM64 US, measured frame by frame; `cargo run --release --example macroscan` measures a
//! schedule over thousands of seeds, `tests/macros.rs` pins the numbers):
//! * The title screen takes Start from its 118th frame on (the intro's sleeps end), the file-select menu
//!   starts 19 frames later and takes Start (or A) as a click on file A, the level starts 34 frames after
//!   that. The click lands on the menu's first frame, before the game's area clock has ticked, so no poke
//!   can replace it (a poke needs a frame the clock moved on); pokes in the fade are harmless. The old
//!   schedule pressed Start from frame 150: 33 frames late.
//! * The intro cutscene (the Lakitu flight, Peach's letter, the pipe) is 1478 frames of fixed length, no input
//!   shortens it; at its end a two-page dialog needs a press on each page (only in dialog state 1, "page
//!   showing"; a press on any other frame is lost).
//! * After a star: the dance, the exit warp, the save menu ("save and continue" / "save and quit" / "continue
//!   without saving", the cursor `gDialogLineNum` moved by the stick with a fresh edge per step), which takes
//!   A only once its timer passes 110; then, on a milestone star, a dialog; then Mario idles.
//! * A pad that is held does nothing twice: a press counts on the edge, so the macros release the frame after.
//!
//! The macros end on the first controllable frame (the old ones waited until Mario stood still for 10 frames
//! or stayed controllable for 60, which the pokes stretch: another 10 to 60 frames).
use anyhow::{Result, bail};
use obrrt_core::{GamePad, St, buttons::{A_BUTTON, START_BUTTON}, read_state};
use sm64pow::SM64MiningGame;

/// The save menu takes A once `gCourseDoneMenuTimer` is above this (ingame_menu.c).
const SAVE_MENU_ARMED: i32 = 110;
/// A stick far enough to scroll a menu (`rawStickY > 60`).
const STICK_UP: i8 = 80;
/// Frames stuck in a level change with no menu before pressing Start (the game waits at its title screen).
const WARP_WAIT: u32 = 30;

/// What the macro presses in `st`; `prev` is the pad played on the frame before (a poke's included), so a
/// button is only pressed when that makes an edge. `warp_wait` counts the frames stuck in a level change.
pub fn pad(st: &St, prev: GamePad, warp_wait: u32) -> GamePad {
    let a_edge = prev.button & A_BUTTON == 0;
    let start_edge = prev.button & START_BUTTON == 0;
    let up_edge = prev.stick_y <= 60;
    // The save menu (and a yes/no dialog): its cursor to line 1, then A.
    let cursor = |line: i8, armed: bool| -> GamePad {
        // The game scrolls before it reads the press (render_save_confirmation, then the A check), so
        // from line 2 one frame does both.
        let up = line > 1 && up_edge;
        let a = armed && a_edge && (line == 1 || (line == 2 && up));
        GamePad::new(if a { A_BUTTON } else { 0 }, 0, if up { STICK_UP } else { 0 })
    };
    if st.in_save_menu() {
        // Before the options show (state 0) the cursor does not read the stick.
        return if st.dialog_state == 1 { cursor(st.menu_line, st.menu_timer > SAVE_MENU_ARMED) } else { GamePad::default() };
    }
    if st.in_dialog() {
        // Only a showing page (state 1) takes a press; the Peach letter (no page) just times out.
        return if st.dialog_state != 1 {
            GamePad::default()
        } else if st.dialog_choice {
            cursor(st.menu_line, true)
        } else {
            GamePad::new(if a_edge { A_BUTTON } else { 0 }, 0, 0)
        };
    }
    // The title screen and the file select (no pokes there): Start on odd frames, the parity the intro's
    // first Start-taking frame (117) has.
    if st.level == 1 || warp_wait >= WARP_WAIT {
        return GamePad::new(if start_edge && st.frame % 2 == 1 { START_BUTTON } else { 0 }, 0, 0);
    }
    GamePad::default()
}

/// Plays the macro until Mario is controllable; returns his state (with the objects).
fn run(game: &mut SM64MiningGame, played: &mut Vec<GamePad>, max_frames: usize, what: &str) -> Result<St> {
    let mut st = read_state(game, played.len() as u32, false);
    // Frames in a row stuck in a level change with no menu (the game waits at its title screen for Start).
    let mut warp_wait = 0u32;
    while !st.controllable() {
        if played.len() >= max_frames {
            bail!("{what}: Mario was not controllable after {} frames (level {}, action {:#x}, play mode {}, menu {} line {} timer {}, dialog {} state {})", played.len(), st.level, st.action, st.play_mode, st.menu_mode, st.menu_line, st.menu_timer, st.dialog, st.dialog_state);
        }
        let prev = played.last().copied().unwrap_or_default();
        played.push(game.step(pad(&st, prev, warp_wait))?.0);
        st = read_state(game, played.len() as u32, false);
        warp_wait = if st.play_mode != 0 && !st.modal() && !st.controllable() { warp_wait + 1 } else { 0 };
    }
    Ok(read_state(game, played.len() as u32, true))
}

/// From power-on to a controllable Mario (the first frame after the intro cutscene).
pub fn boot(game: &mut SM64MiningGame, played: &mut Vec<GamePad>, max_frames: usize) -> Result<St> {
    run(game, played, max_frames, "boot")
}

/// From a parent's end state (a star's dance) to a controllable Mario.
pub fn settle(game: &mut SM64MiningGame, played: &mut Vec<GamePad>, max_frames: usize) -> Result<St> {
    run(game, played, max_frames, "settle")
}
