//! The Super Mario 64 adapter: which memory the perturbation reads (the
//! clock and the state digest), per game build. Native code reads the regions
//! from the headless build; the browser reads the same regions (at the visual
//! build's addresses) and hands the bytes to this code compiled to WASM.

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
        u32_at(rest) << 32 | u16_at(rest + 4) << 16 | u16_at(rest + 6),
        u16_at(l + LAKITU_YAW) << 48 | u16_at(rest + 8) << 16 | u16_at(m + MARIO_NUM_STARS),
    ];
    View { clock, digest }
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
}
