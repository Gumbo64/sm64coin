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

/// Result of comparing a Mupen64Plus per-poll RAM dump (`M64_DUMP`, see `tools/mupen64plus/m64_input.c`)
/// with the port.
#[derive(Debug, Default)]
pub struct DumpReport {
    pub polls: usize,
    pub differing: usize,
    /// The first few differing polls: (poll, port frames played, what differs).
    pub first: Vec<(usize, usize, String)>,
}

/// Plays `pads` in the port and compares, at every poll in `dump`, Mario (action, position, velocity,
/// health, stars), the RNG word and the global timer with Mupen64Plus' RDRAM. The record of poll p is the
/// port after p - 2 frames (the `.mupen64plus.m64` variant has one leading zero poll). `pool` says the
/// dump also holds the 240-object pool (not compared here). Polls before the game booted (p < 3) are skipped.
pub fn compare_dump(pads: &[sm64_binds::GamePad], dump: &[u8], pool: bool) -> Result<DumpReport> {
    let rec = 16 + 0xC8 + 8 + if pool { 240 * 0x260 } else { 0 };
    let mut g = crate::export::fresh_game()?;
    let mut done = 0usize;
    let mut rep = DumpReport::default();
    for r in dump.chunks_exact(rec) {
        let poll = i64::from_le_bytes(r[..8].try_into().unwrap()) as usize;
        if poll < 3 {
            continue;
        }
        while done < poll - 2 && done < pads.len() {
            g.step_game(pads[done])?;
            done += 1;
        }
        if poll - 2 > pads.len() {
            break;
        }
        rep.polls += 1;
        let m = &r[16..16 + 0xC8];
        let be = |o: usize| u32::from_be_bytes(m[o..o + 4].try_into().unwrap());
        let f = |o: usize| f32::from_bits(be(o));
        let theirs = MarioSnap {
            action: be(12),
            pos: [f(60), f(64), f(68)],
            vel: [f(72), f(76), f(80)],
            health: (be(172) & 0xFFFF) as u16,
            stars: (be(168) & 0xFFFF) as i16,
            timer: 0,
        };
        let ours = from_port(&g)?;
        let rng_n64 = u16::from_be_bytes([r[16 + 0xC8], r[17 + 0xC8]]);
        let b = sm64pow::game::read_region_bytes(&g)?;
        let rng_port = u16::from_le_bytes([b[2 + 176 + 126 + 4], b[2 + 176 + 126 + 5]]);
        let timer_n64 = u32::from_be_bytes(r[16 + 0xC8 + 4..16 + 0xC8 + 8].try_into().unwrap());
        let mut what = vec![];
        if !same_mario(&theirs, &ours) {
            what.push(format!("Mario: n64 {theirs:?} port {ours:?}"));
        }
        if rng_n64 != rng_port {
            what.push(format!("rng n64 {rng_n64:#06x} port {rng_port:#06x}"));
        }
        if timer_n64 != ours.timer {
            what.push(format!("timer n64 {timer_n64} port {}", ours.timer));
        }
        if !what.is_empty() {
            rep.differing += 1;
            if rep.first.len() < 5 {
                rep.first.push((poll, poll - 2, what.join("; ")));
            }
        }
    }
    Ok(rep)
}
