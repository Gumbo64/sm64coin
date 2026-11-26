//! The macros a bot plays before its own play starts: [`boot`] from power-on (title screen, file select,
//! the intro cutscene and its dialog) and [`settle`] from a parent's end state (the star dance, the save
//! menu, the milestone dialog), each up to the first frame Mario is controllable.
//!
//! State-reactive and fast: every frame the pad is a function of the game's state (the open menu or
//! dialog, its cursor, the last pad actually played, which a poke may have replaced) and of nothing else,
//! never of the seed. From power-on Mario is controllable at frame 1646-1649 (the old fixed schedule, 1680
//! frames and then waiting for him to stand still: 1690-1816); the intro cutscene itself is 1478 frames of
//! game time that no input shortens. After a star about 355 frames, most of it the save menu's own timer.
//! They survive any poke pattern the rules allow: a lost press is retried the next frame, a moved cursor
//! is moved back, "save and quit" is recovered from. The same logic runs in the `automine` miner and the
//! web miner (`sm64coin_miner::macros`; a test here checks the two play the very same pads).
//!
//! ```no_run
//! # use sm64coin_sdk::*;
//! # fn demo(env: &mut MiningEnv) -> anyhow::Result<()> {
//! macros::boot(env)?; // or macros::settle(env)? from a parent's end state
//! // ... your bot plays from here, Mario is in control.
//! # Ok(()) }
//! ```
//! Memory addresses are those of the pinned game build (`sm64pow::GAME_BUILD`, the headless build).
use crate::{GamePad, MiningEnv, buttons::{A_BUTTON, START_BUTTON}};
use anyhow::{Result, bail};
use sm64pow::sm64_binds::SM64Game;

// Addresses in the headless build (the layout of objective-rrt-v2's core/src/layout.rs).
const ADDR_LEVEL: u32 = 0x7ba0a2; // gCurrLevelNum
const ADDR_MENU_MODE: u32 = 0x7bd210; // gMenuMode
const ADDR_DIALOG_ID: u32 = 0x7bd208; // gDialogID
const ADDR_DIALOG_LINE: u32 = 0x7bd20a; // gDialogLineNum
const ADDR_DIALOG_STATE: u32 = 0xb09fd5; // gDialogBoxState
const ADDR_DIALOG_RESPONSE: u32 = 0xb09fde; // gLastDialogResponse
const ADDR_DONE_TIMER: u32 = 0xb0a000; // gCourseDoneMenuTimer
const ADDR_PLAY_MODE: u32 = 0xb0a0ec; // sCurrPlayMode
const ADDR_MARIO_ACTION: u32 = 0xb0a010 + 12; // gMarioStates[0].action
const ADDR_ACT_LOADED: u32 = 0xbc5c55; // sLoadedActNum
const ADDR_ACT_TIMER: u32 = 0xbc5c58; // sActSelectorMenuTimer
const MENU_MODE_COURSE_COMPLETE: i16 = 2;
const ACT_GROUP_MASK: u32 = 0x1c0;
const ACT_GROUP_CUTSCENE: u32 = 0x100;

/// The save menu takes A once its timer is above this.
const SAVE_MENU_ARMED: i32 = 110;
/// A stick far enough to scroll a menu (`rawStickY > 60`).
const STICK_UP: i8 = 80;
/// Frames stuck in a level change with no menu before pressing Start (the game waits at its title screen).
const WARP_WAIT: u32 = 30;

/// What the macros read of the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View {
    pub level: i16,
    pub play_mode: i16,
    pub action: u32,
    pub menu_mode: i16,
    pub dialog: i16,
    pub dialog_state: i8,
    pub dialog_choice: bool,
    pub menu_line: i8,
    pub menu_timer: i32,
    pub act_select: bool,
}

impl View {
    pub fn read(game: &SM64Game) -> Result<View> {
        let mut b = [0u8; 4];
        let mut rd = |addr: u32, n: usize| -> Result<i64> {
            game.read_memory(addr, &mut b[..n])?;
            Ok(match n {
                1 => b[0] as i8 as i64,
                2 => i16::from_le_bytes([b[0], b[1]]) as i64,
                _ => i32::from_le_bytes(b) as i64,
            })
        };
        let menu_mode = rd(ADDR_MENU_MODE, 2)? as i16;
        let act_timer = rd(ADDR_ACT_TIMER, 4)? as i32;
        let act_select = rd(ADDR_ACT_LOADED, 1)? == 0 && act_timer > 0;
        let in_save_menu = menu_mode == MENU_MODE_COURSE_COMPLETE;
        Ok(View {
            level: rd(ADDR_LEVEL, 2)? as i16,
            play_mode: rd(ADDR_PLAY_MODE, 2)? as i16,
            action: rd(ADDR_MARIO_ACTION, 4)? as u32,
            menu_mode,
            dialog: rd(ADDR_DIALOG_ID, 2)? as i16,
            dialog_state: rd(ADDR_DIALOG_STATE, 1)? as i8,
            dialog_choice: rd(ADDR_DIALOG_RESPONSE, 1)? == 1,
            menu_line: rd(ADDR_DIALOG_LINE, 1)? as i8,
            menu_timer: if in_save_menu { rd(ADDR_DONE_TIMER, 4)? as i32 } else if act_select { act_timer } else { 0 },
            act_select,
        })
    }

    fn in_cutscene_group(&self) -> bool {
        self.action & ACT_GROUP_MASK == ACT_GROUP_CUTSCENE
    }

    /// In a dialog that still holds Mario (a closing one no longer does once Mario plays again).
    pub fn in_dialog(&self) -> bool {
        let released = self.dialog >= 0 && self.dialog_state == 3 && self.play_mode == 0 && self.action != 0 && !self.in_cutscene_group();
        self.dialog >= 0 && !released
    }

    pub fn in_save_menu(&self) -> bool {
        self.menu_mode == MENU_MODE_COURSE_COMPLETE
    }

    /// Normal play, Mario out of any cutscene, menu or dialog.
    pub fn controllable(&self) -> bool {
        self.play_mode == 0 && self.action != 0 && !self.in_cutscene_group() && !(self.in_dialog() || self.in_save_menu() || self.act_select)
    }
}

/// What the macro presses in `v` on frame `frame` (the index of the frame about to be played); `prev` is the
/// pad played on the frame before (a poke's included), so a button is only pressed when that makes an edge;
/// `warp_wait` counts the frames stuck in a level change.
pub fn pad(v: &View, prev: GamePad, warp_wait: u32, frame: usize) -> GamePad {
    let a_edge = prev.button & A_BUTTON == 0;
    let start_edge = prev.button & START_BUTTON == 0;
    let up_edge = prev.stick_y <= 60;
    // The save menu (and a yes/no dialog): the cursor to line 1, then A. The game scrolls before it reads the
    // press, so from line 2 one frame does both.
    let cursor = |line: i8, armed: bool| -> GamePad {
        let up = line > 1 && up_edge;
        let a = armed && a_edge && (line == 1 || (line == 2 && up));
        GamePad::new(if a { A_BUTTON } else { 0 }, 0, if up { STICK_UP } else { 0 })
    };
    if v.in_save_menu() {
        return if v.dialog_state == 1 { cursor(v.menu_line, v.menu_timer > SAVE_MENU_ARMED) } else { GamePad::default() };
    }
    if v.in_dialog() {
        return if v.dialog_state != 1 {
            GamePad::default()
        } else if v.dialog_choice {
            cursor(v.menu_line, true)
        } else {
            GamePad::new(if a_edge { A_BUTTON } else { 0 }, 0, 0)
        };
    }
    // The title screen and the file select (no pokes there): Start on odd frames, the parity of the intro's first
    // Start-taking frame (117).
    if v.level == 1 || warp_wait >= WARP_WAIT {
        return GamePad::new(if start_edge && frame % 2 == 1 { START_BUTTON } else { 0 }, 0, 0);
    }
    GamePad::default()
}

/// Plays the macro until Mario is controllable (at most `max_frames` frames of the run in all); returns the
/// frames played by this call.
fn run(env: &mut MiningEnv, max_frames: usize, what: &str) -> Result<usize> {
    let first = env.frame();
    let mut warp_wait = 0u32;
    let mut v = View::read(env.game_mut())?;
    while !v.controllable() {
        if env.frame() >= max_frames.min(env.frame_cap()) {
            bail!("{what}: Mario was not controllable after {} frames ({v:?})", env.frame());
        }
        let prev = env.pads().last().copied().unwrap_or_default();
        env.step(pad(&v, prev, warp_wait, env.frame()))?;
        v = View::read(env.game_mut())?;
        warp_wait = if v.play_mode != 0 && !(v.in_dialog() || v.in_save_menu() || v.act_select) && !v.controllable() { warp_wait + 1 } else { 0 };
    }
    Ok(env.frame() - first)
}

/// From power-on to the first controllable frame (frame 1646-1649 under the rules' pokes). Returns the frames
/// played. Needs a power-on task and a fresh env (or any env that has not left the title screen).
pub fn boot(env: &mut MiningEnv) -> Result<usize> {
    let cap = env.frame_cap();
    run(env, cap, "boot")
}

/// From a parent's end state (a star's dance) to the first controllable frame (about 355 frames later).
/// Returns the frames played.
pub fn settle(env: &mut MiningEnv) -> Result<usize> {
    let cap = env.frame_cap();
    run(env, cap, "settle")
}
