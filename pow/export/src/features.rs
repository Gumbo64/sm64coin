//! Per-frame state features read from the game's RAM (the same regions the
//! perturbation reads: `sm64coin-perturb/src/sm64.rs`).
//!
//! A record is [`COLUMNS`] little-endian 32-bit words (`f32`, `u32` or `i32`
//! per column). A block with N frames has N+1 records: record 0 is the state
//! before frame 0, record k the state after frame k-1 was played. (The last
//! record of a block is the first of its child.)

use anyhow::Result;
use sm64_binds::SM64Game;
use sm64pow::game::{features, read_region_bytes};

pub const COLUMNS: &[(&str, &str, &str)] = &[
    ("global_timer", "u32", "gGlobalTimer (frames since power-on, wraps)"),
    ("action", "u32", "Mario action (ACT_*)"),
    ("prev_action", "u32", "previous Mario action"),
    ("flags", "u32", "MarioState.flags (MARIO_* bits)"),
    ("action_state", "u32", "MarioState.actionState"),
    ("action_timer", "u32", "MarioState.actionTimer"),
    ("input", "u32", "MarioState.input (INPUT_* bits derived from the pad)"),
    ("intended_yaw", "u32", "intended yaw from the stick (s16 as u16)"),
    ("intended_mag", "f32", "intended magnitude from the stick"),
    ("face_yaw", "u32", "Mario's facing yaw (s16 as u16)"),
    ("cam_yaw", "u32", "Lakitu camera yaw (s16 as u16)"),
    ("pos_x", "f32", "Mario position"),
    ("pos_y", "f32", ""),
    ("pos_z", "f32", ""),
    ("vel_x", "f32", "Mario velocity"),
    ("vel_y", "f32", ""),
    ("vel_z", "f32", ""),
    ("fwd_vel", "f32", "forward speed"),
    ("health", "u32", "health (0x100 per wedge)"),
    ("coins", "u32", "coins in this level"),
    ("stars", "u32", "stars"),
    ("level", "u32", "gCurrLevelNum"),
    ("area", "u32", "gCurrAreaIndex"),
    ("rng", "u32", "gRandomSeed16 (the game's RNG state)"),
    ("cam_pos_x", "f32", "Lakitu camera position"),
    ("cam_pos_y", "f32", ""),
    ("cam_pos_z", "f32", ""),
];

pub const RECORD_BYTES: usize = COLUMNS.len() * 4;

// Offsets inside `read_region_bytes`: [clock 2][Mario 176][Lakitu 126][timer 4][rng 2][level 2][area 2].
const M: usize = 2;
const L: usize = M + 176;
const REST: usize = L + 126;

/// Mario's struct (include/types.h): pos 60, vel 72, forwardVel 84, numCoins 168 (s16), numStars 170 (s16).
/// LakituState (camera.h): curPos 12.
pub fn record(game: &SM64Game) -> Result<[u8; RECORD_BYTES]> {
    let b = read_region_bytes(game)?;
    let f = features(&b);
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as u32;
    let f32_at = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let words: [u32; COLUMNS.len()] = [
        f.timer,
        f.action,
        f.prev_action,
        f.flags,
        f.action_state as u32,
        f.action_timer as u32,
        f.input as u32,
        f.intended_yaw as u32,
        (f.intended_mag as f32 / 256.0).to_bits(),
        f.face_yaw as u32,
        f.cam_yaw as u32,
        f32_at(M + 60).to_bits(),
        f32_at(M + 64).to_bits(),
        f32_at(M + 68).to_bits(),
        f32_at(M + 72).to_bits(),
        f32_at(M + 76).to_bits(),
        f32_at(M + 80).to_bits(),
        f32_at(M + 84).to_bits(),
        f.health as u32,
        u16_at(M + 168),
        u16_at(M + 170),
        f.level as u32,
        f.area as u32,
        u16_at(REST + 4),
        f32_at(L + 12).to_bits(),
        f32_at(L + 16).to_bits(),
        f32_at(L + 20).to_bits(),
    ];
    let mut out = [0u8; RECORD_BYTES];
    for (i, w) in words.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    Ok(out)
}

/// The JSON schema written next to the feature files.
pub fn schema() -> serde_json::Value {
    serde_json::json!({
        "format": "sm64coin-features-v1",
        "endianness": "little",
        "record_bytes": RECORD_BYTES,
        "records_per_block": "frames + 1 (record 0: before frame 0; record k: after frame k-1)",
        "columns": COLUMNS.iter().map(|(n, t, d)| serde_json::json!({"name": n, "dtype": t, "doc": d})).collect::<Vec<_>>(),
        "numpy": format!("np.dtype([{}])", COLUMNS.iter().map(|(n, t, _)| format!("('{n}','<{}4')", match *t { "f32" => "f", "i32" => "i", _ => "u" })).collect::<Vec<_>>().join(",")),
    })
}
