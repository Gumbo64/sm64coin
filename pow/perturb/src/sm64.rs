//! The Super Mario 64 adapter: which memory the perturbation reads (the
//! clock and the state digest), per game build. Native code reads the regions
//! from the headless build; the browser reads the same regions (at the visual
//! build's addresses) and hands the bytes to this code compiled to WASM.
//!
//! Also here: [`Progress`] (what the rules count: stars, Bowser keys, the
//! Grand Star) and [`Features`] (what the state-aware codecs condition on).
//! The addresses are generated (sm64_addrs.rs, pow/tools/gen_sm64_addrs.py).
//!
//! Read next: pow/sm64pow/src/emulator.rs, then game.rs.

#[allow(non_snake_case)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Addrs {
    pub gMarioStates: u32,
    pub gLakituState: u32,
    pub gGlobalTimer: u32,
    pub gRandomSeed16: u32,
    pub gAreaUpdateCounter: u32,
    pub gCurrLevelNum: u32,
    pub gCurrAreaIndex: u32,
    pub gSaveBuffer: u32,
    pub gCurrSaveFileNum: u32,
}

mod addrs {
    use super::Addrs;
    include!("sm64_addrs.rs");
}
pub use addrs::{HEADLESS, VISUAL};

// Struct offsets (include/types.h, camera.h; the same in every wasm32 build).
const MARIO_ACTION: usize = 12;
const MARIO_FACE_ANGLE: usize = 44;
const MARIO_POS: usize = 60;
const MARIO_VEL: usize = 72;
const MARIO_FORWARD_VEL: usize = 84;
const MARIO_NUM_COINS: usize = 168;
const MARIO_NUM_STARS: usize = 170;
const MARIO_HEALTH: usize = 174;
const MARIO_SIZE: u32 = 176;
const LAKITU_YAW: usize = 124;
const LAKITU_SIZE: u32 = 126;

/// The regions to read, in order: (address, length).
pub fn regions(a: &Addrs) -> [(u32, u32); 7] {
    [
        (a.gAreaUpdateCounter, 2), // the clock: moves only when the level simulates
        (a.gMarioStates, MARIO_SIZE),
        (a.gLakituState, LAKITU_SIZE),
        (a.gGlobalTimer, 4),
        (a.gRandomSeed16, 2), // the game's own RNG: moves with almost everything in the level
        (a.gCurrLevelNum, 2),
        (a.gCurrAreaIndex, 2),
    ]
}

/// Total bytes of [`regions`].
pub fn regions_len(a: &Addrs) -> usize {
    regions(a).iter().map(|r| r.1 as usize).sum()
}

/// What the perturbation sees of the game: its clock and a digest of state
/// (values only, no pointers, so every build agrees).
pub struct View {
    pub clock: u64,
    pub digest: [u64; 7],
}

/// Computes the view from the regions' bytes, concatenated in [`regions`]
/// order.
pub fn view(bytes: &[u8]) -> View {
    let u16_at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]) as u64;
    let u32_at = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]) as u64;
    let m = 2; // Mario's region
    let l = m + MARIO_SIZE as usize;
    let rest = l + LAKITU_SIZE as usize;
    let clock = u16_at(0);
    let digest = [
        u32_at(m + MARIO_POS) << 32 | u32_at(m + MARIO_POS + 4),
        u32_at(m + MARIO_POS + 8) << 32 | u32_at(m + MARIO_FORWARD_VEL),
        u32_at(m + MARIO_VEL) << 32 | u32_at(m + MARIO_VEL + 4),
        u32_at(m + MARIO_VEL + 8) << 32 | u32_at(m + MARIO_ACTION),
        u16_at(m + MARIO_FACE_ANGLE) << 48 | u16_at(m + MARIO_FACE_ANGLE + 2) << 32 | u16_at(m + MARIO_HEALTH) << 16 | u16_at(m + MARIO_NUM_COINS),
        // Not gGlobalTimer (at `rest`): it counts paused frames too, so it
        // would let a pause of any length reroll every later poke.
        u16_at(rest + 4) << 16 | u16_at(rest + 6),
        u16_at(l + LAKITU_YAW) << 48 | u16_at(rest + 8) << 16 | u16_at(m + MARIO_NUM_STARS),
    ];
    View { clock, digest }
}


// --- Features: what a state-aware proof codec may condition on. Integers only
// (the decode path is consensus): floats in memory are converted by bit
// manipulation, truncating, so every platform agrees.

const MARIO_INPUT: usize = 2;
const MARIO_FLAGS: usize = 4;
const MARIO_PREV_ACTION: usize = 16;
const MARIO_ACTION_STATE: usize = 24;
const MARIO_ACTION_TIMER: usize = 26;
const MARIO_INTENDED_MAG: usize = 32;
const MARIO_INTENDED_YAW: usize = 36;

/// `f32` bits to a fixed-point integer with `frac` fraction bits, truncating
/// toward zero and saturating (NaN and infinities saturate): no float maths.
pub fn f32_fx(bits: u32, frac: u32) -> i32 {
    let neg = bits >> 31 != 0;
    let e = ((bits >> 23) & 0xff) as i32;
    if e == 0 {
        return 0;
    }
    let m = ((bits & 0x7f_ffff) | 0x80_0000) as u64;
    let shift = e - 127 - 23 + frac as i32;
    let mag: u64 = if shift >= 0 {
        if shift > 31 { u64::MAX } else { m << shift }
    } else if -shift >= 64 {
        0
    } else {
        m >> (-shift)
    };
    let mag = mag.min(i32::MAX as u64) as i32;
    if neg { -mag } else { mag }
}

/// The game state a state-aware codec conditions on, read (from the same
/// bytes as [`view`]) before a frame is played.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Features {
    pub action: u32,
    pub prev_action: u32,
    pub flags: u32,
    pub input: u16,
    pub action_state: u16,
    pub action_timer: u16,
    pub intended_yaw: u16,
    /// Last frame's stick magnitude, 8 fraction bits.
    pub intended_mag: i32,
    pub face_yaw: u16,
    pub cam_yaw: u16,
    /// Position, whole units.
    pub pos: [i32; 3],
    /// Forward speed and velocity, 8 fraction bits.
    pub fwd: i32,
    pub vel: [i32; 3],
    pub health: u16,
    pub level: u16,
    pub area: u16,
    pub timer: u32,
}

/// [`Features`] from the regions' bytes, concatenated in [`regions`] order.
pub fn features(bytes: &[u8]) -> Features {
    let u16_at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let m = 2;
    let l = m + MARIO_SIZE as usize;
    let rest = l + LAKITU_SIZE as usize;
    let fx = |o: usize, f: u32| f32_fx(u32_at(m + o), f);
    Features {
        action: u32_at(m + MARIO_ACTION),
        prev_action: u32_at(m + MARIO_PREV_ACTION),
        flags: u32_at(m + MARIO_FLAGS),
        input: u16_at(m + MARIO_INPUT),
        action_state: u16_at(m + MARIO_ACTION_STATE),
        action_timer: u16_at(m + MARIO_ACTION_TIMER),
        intended_yaw: u16_at(m + MARIO_INTENDED_YAW),
        intended_mag: fx(MARIO_INTENDED_MAG, 8),
        face_yaw: u16_at(m + MARIO_FACE_ANGLE + 2),
        cam_yaw: u16_at(l + LAKITU_YAW),
        pos: [fx(MARIO_POS, 0), fx(MARIO_POS + 4, 0), fx(MARIO_POS + 8, 0)],
        fwd: fx(MARIO_FORWARD_VEL, 8),
        vel: [fx(MARIO_VEL, 8), fx(MARIO_VEL + 4, 8), fx(MARIO_VEL + 8, 8)],
        health: u16_at(m + MARIO_HEALTH),
        level: u16_at(rest + 6),
        area: u16_at(rest + 8),
        timer: u32_at(rest),
    }
}

// --- Progress: what a block must add (a star, a Bowser key, the Grand Star).

// include/sm64.h, src/game/save_file.h (the same in every wasm32 build).
const ACT_JUMBO_STAR_CUTSCENE: u32 = 0x0000_1909;
const SAVE_FILE_SIZE: usize = 0x38; // struct SaveFile
const SAVE_FILES: usize = 4; // NUM_SAVE_FILES, two copies each
const SAVE_FLAGS: usize = 8; // SaveFile.flags
const COURSE_STARS: usize = 12; // SaveFile.courseStars[25]
const HAVE_KEY_1: u32 = 1 << 4;
const HAVE_KEY_2: u32 = 1 << 5;
const UNLOCKED_BASEMENT_DOOR: u32 = 1 << 6;
const UNLOCKED_UPSTAIRS_DOOR: u32 = 1 << 7;
/// Stars needed before the Grand Star counts (the 70-star category).
pub const GRAND_STAR_MIN_STARS: u32 = 70;

/// The regions [`progress`] reads, in order: Mario, the save files (first
/// copies), the current file's number.
pub fn progress_regions(a: &Addrs) -> [(u32, u32); 3] {
    [
        (a.gMarioStates, MARIO_SIZE),
        (a.gSaveBuffer, (SAVE_FILE_SIZE * 2 * SAVE_FILES) as u32),
        (a.gCurrSaveFileNum, 2),
    ]
}

pub fn progress_regions_len(a: &Addrs) -> usize {
    progress_regions(a).iter().map(|r| r.1 as usize).sum()
}

/// How far into the game a state is: one point per star, per Bowser key
/// (held, or already used on its door, so it never goes back down) and for
/// the Grand Star (with at least [`GRAND_STAR_MIN_STARS`] stars). A block's
/// run must add exactly one point; the Grand Star ends the cycle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub stars: u32,
    pub keys: u32,
    pub grand_star: bool,
    /// Which stars (bookkeeping, not consensus): index 0 the castle's secret
    /// stars, 1..=25 course N's stars (bit k = star k+1).
    pub star_flags: [u8; 26],
}

/// What a block added (see [`event`]): for logging per-star run times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Course 0 is the castle (secret stars), 1..=25 the courses; star 1..=7.
    Star { course: u8, star: u8 },
    Key(u8),
    GrandStar,
}

impl Event {
    /// A short stable name: "c09s1" (course 9 star 1), "c00s3" (castle), "key1", "grand".
    pub fn name(&self) -> String {
        match self {
            Event::Star { course, star } => format!("c{course:02}s{star}"),
            Event::Key(k) => format!("key{k}"),
            Event::GrandStar => "grand".into(),
        }
    }
}

/// The progress made between `before` and `after` (the first difference).
pub fn event(before: &Progress, after: &Progress) -> Option<Event> {
    if after.grand_star && !before.grand_star {
        return Some(Event::GrandStar);
    }
    for c in 0..26 {
        let new = after.star_flags[c] & !before.star_flags[c];
        if new != 0 {
            return Some(Event::Star { course: c as u8, star: new.trailing_zeros() as u8 + 1 });
        }
    }
    if after.keys > before.keys {
        return Some(Event::Key(after.keys as u8));
    }
    None
}

impl Progress {
    pub fn points(&self) -> u32 {
        self.stars + self.keys + self.grand_star as u32
    }
}

/// [`Progress`] from the bytes of [`progress_regions`], concatenated.
pub fn progress(bytes: &[u8]) -> Progress {
    let u16_at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]) as u32;
    let u32_at = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let stars = u16_at(MARIO_NUM_STARS);
    let action = u32_at(MARIO_ACTION);
    let saves = MARIO_SIZE as usize;
    let file = u16_at(saves + SAVE_FILE_SIZE * 2 * SAVE_FILES) as usize;
    let flags = if (1..=SAVE_FILES).contains(&file) { u32_at(saves + (file - 1) * 2 * SAVE_FILE_SIZE + SAVE_FLAGS) } else { 0 };
    let keys = (flags & (HAVE_KEY_1 | UNLOCKED_BASEMENT_DOOR) != 0) as u32 + (flags & (HAVE_KEY_2 | UNLOCKED_UPSTAIRS_DOOR) != 0) as u32;
    let mut star_flags = [0u8; 26];
    if (1..=SAVE_FILES).contains(&file) {
        let f = saves + (file - 1) * 2 * SAVE_FILE_SIZE;
        star_flags[0] = ((flags >> 24) & 0x7f) as u8;
        for c in 0..25 {
            star_flags[c + 1] = bytes[f + COURSE_STARS + c] & 0x7f;
        }
    }
    Progress { stars, keys, grand_star: action == ACT_JUMBO_STAR_CUTSCENE && stars >= GRAND_STAR_MIN_STARS, star_flags }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_cover_what_view_reads() {
        for a in [HEADLESS, VISUAL] {
            let n = regions_len(&a);
            assert_eq!(view(&vec![1u8; n]).clock, 0x0101);
        }
        assert_ne!(HEADLESS, VISUAL, "the two builds lay memory out differently");
    }

    #[test]
    fn progress_counts_stars_keys_and_the_grand_star() {
        let n = progress_regions_len(&HEADLESS);
        let mut b = vec![0u8; n];
        assert_eq!(progress(&b).points(), 0);
        b[MARIO_NUM_STARS] = 3;
        let file_num = n - 2;
        b[file_num] = 2; // file 2 (1-based)
        let flags = MARIO_SIZE as usize + 2 * SAVE_FILE_SIZE + SAVE_FLAGS; // file 2, first copy
        b[flags] = UNLOCKED_BASEMENT_DOOR as u8 | HAVE_KEY_2 as u8;
        b[MARIO_SIZE as usize + 2 * SAVE_FILE_SIZE + COURSE_STARS + 8] = 0b101; // course 9: stars 1 and 3
        let p = progress(&b);
        assert_eq!((p.stars, p.keys, p.grand_star), (3, 2, false));
        assert_eq!(p.star_flags[9], 0b101);
        let mut q = p;
        q.star_flags[9] |= 0b10;
        assert_eq!(event(&p, &q), Some(Event::Star { course: 9, star: 2 }));
        assert_eq!(event(&p, &q).unwrap().name(), "c09s2");
        b[MARIO_ACTION..MARIO_ACTION + 4].copy_from_slice(&ACT_JUMBO_STAR_CUTSCENE.to_le_bytes());
        assert!(!progress(&b).grand_star, "needs 70 stars");
        b[MARIO_NUM_STARS] = 70;
        assert_eq!(progress(&b).points(), 73);
    }
}
