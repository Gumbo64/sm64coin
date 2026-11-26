//! The emulator (layer 2): the ROM, the consensus game build, and fresh games.
//!
//! The game is sm64-binds' headless WASM build of Super Mario 64, run by
//! wasmer. [`init`] loads the ROM once per process and refuses any game build
//! but [`GAME_BUILD`]: a different build is a different game. [`generator`]
//! makes game instances (each one powered on).
//!
//! Read next: game.rs.

use anyhow::{Result, anyhow};
use sm64_binds::SM64GameGenerator;
use std::sync::OnceLock;

static GENERATOR: OnceLock<SM64GameGenerator> = OnceLock::new();

/// Loads the ROM once per process. Later calls are no-ops.
/// The game build consensus runs: the SHA-1 of the headless WASM's code
/// (sm64-binds' build id). A node or miner with any other build would see a
/// different game, so [`init`] refuses it. Changing the game build is a hard
/// fork: update this together with the address tables (pow/tools/gen_sm64_addrs.py).
pub const GAME_BUILD: &str = "4a657a093b061633a619e4129735d77d23e24c2f";

/// The build id of a game (from its save-state header, bytes 12..32).
pub fn game_build_id(g: &SM64GameGenerator) -> Result<String> {
    let st = g.create_game()?.save_state();
    Ok(st.as_bytes()[12..32].iter().map(|b| format!("{b:02x}")).collect())
}

pub fn init(rom: Vec<u8>) -> Result<&'static SM64GameGenerator> {
    if let Some(g) = GENERATOR.get() {
        return Ok(g);
    }
    let g = SM64GameGenerator::new(rom)?;
    let id = game_build_id(&g)?;
    anyhow::ensure!(
        id == GAME_BUILD,
        "this sm64-binds game build is {id}, but SM64Coin's consensus runs {GAME_BUILD}: build against the pinned sm64-binds (see pow/Cargo.toml)"
    );
    let _ = GENERATOR.set(g);
    Ok(GENERATOR.get().unwrap())
}

/// The ROM path: `$SM64COIN_ROM`, else `baserom.us.z64` in the working
/// directory, else `~/.sm64coin/baserom.us.z64`.
pub fn default_rom_path() -> Option<String> {
    if let Ok(p) = std::env::var("SM64COIN_ROM") {
        return Some(p);
    }
    let mut cands = vec!["baserom.us.z64".to_string()];
    if let Ok(h) = std::env::var("HOME") {
        cands.push(format!("{h}/.sm64coin/baserom.us.z64"));
    }
    cands.into_iter().find(|p| std::path::Path::new(p).exists())
}

pub fn init_default() -> Result<&'static SM64GameGenerator> {
    if let Some(g) = GENERATOR.get() {
        return Ok(g);
    }
    let path = default_rom_path().ok_or_else(|| {
        anyhow!("no SM64 ROM: set SM64COIN_ROM or put baserom.us.z64 in the working directory")
    })?;
    init(std::fs::read(&path).map_err(|e| anyhow!("reading ROM {path}: {e}"))?)
}

pub fn generator() -> Result<&'static SM64GameGenerator> {
    init_default()
}
