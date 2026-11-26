//! Comparing the port's replay of a cycle m64 with a stock Mupen64Plus run of
//! the same m64 (`tools/mupen64plus/`): Mario's state in a Mupen64Plus `.st`
//! against the port after the same number of polls.
//!
//! US ROM addresses (decomp symbols): `gMarioStates` 0x8033B170,
//! `gGlobalTimer` 0x8032D5D4.

use anyhow::{Result, anyhow};
use sm64_binds::SM64Game;
use sm64_binds::mupen::MupenState;

const MARIO: u32 = 0x8033B170;
const GLOBAL_TIMER: u32 = 0x8032D5D4;

#[derive(Debug, Clone, PartialEq)]
pub struct MarioSnap {
    pub action: u32,
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    pub health: u16,
    pub stars: i16,
    pub timer: u32,
}

pub fn from_state(st: &MupenState) -> Result<MarioSnap> {
    let f = |o: u32| st.f32(MARIO + o).ok_or_else(|| anyhow!("address out of RDRAM"));
    Ok(MarioSnap {
        action: st.u32(MARIO + 12).ok_or_else(|| anyhow!("action"))?,
        pos: [f(60)?, f(64)?, f(68)?],
        vel: [f(72)?, f(76)?, f(80)?],
        health: st.u16(MARIO + 174).ok_or_else(|| anyhow!("health"))?,
        stars: st.s16(MARIO + 170).ok_or_else(|| anyhow!("stars"))?,
        timer: st.u32(GLOBAL_TIMER).ok_or_else(|| anyhow!("timer"))?,
    })
}

/// The port's Mario (from the headless RAM: the same struct, wasm32 addresses).
pub fn from_port(g: &SM64Game) -> Result<MarioSnap> {
    let b = sm64pow::game::read_region_bytes(g)?;
    let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let f = |o: usize| f32::from_bits(u32_at(o));
    let m = 2;
    Ok(MarioSnap {
        action: u32_at(m + 12),
        pos: [f(m + 60), f(m + 64), f(m + 68)],
        vel: [f(m + 72), f(m + 76), f(m + 80)],
        health: u16::from_le_bytes([b[m + 174], b[m + 175]]),
        stars: i16::from_le_bytes([b[m + 170], b[m + 171]]),
        timer: u32_at(2 + 176 + 126),
    })
}

/// Whether Mupen's Mario is the port's, ignoring `timer` (which is offset by one poll).
pub fn same_mario(a: &MarioSnap, b: &MarioSnap) -> bool {
    a.action == b.action && a.pos == b.pos && a.vel == b.vel && a.health == b.health && a.stars == b.stars
}
