//! The game adapter: what the perturbation reads from Super Mario 64, and a
//! game bundled with its perturbation ([`MiningGame`]).
//!
//! The adapter names a few memory regions; native code reads them from the
//! game, the browser copies the same bytes out of its build's memory and
//! hands them to this code compiled to WASM. Both then compute the clock and
//! digest with [`view`]. Another game needs another adapter (its regions,
//! clock and digest) and nothing else.

use crate::{Params, Seed};
use anyhow::Result;
use sm64_binds::{GamePad, GameState, RngConfig, SM64Game, SaveState};
pub use sm64coin_perturb::sm64::{Addrs, HEADLESS, VISUAL, View, regions, regions_len, view};
pub use sm64coin_perturb::{Pad, PokeConfig, Perturber};

pub fn to_pad(p: GamePad) -> Pad {
    Pad { button: p.button, stick_x: p.stick_x, stick_y: p.stick_y }
}

pub fn from_pad(p: Pad) -> GamePad {
    GamePad::new(p.button, p.stick_x, p.stick_y)
}

pub fn poke_config(r: &RngConfig) -> PokeConfig {
    PokeConfig {
        window_length: r.window_length,
        random_amount: r.random_amount,
        burst_length: r.random_burst_length,
        a_prob: r.a_prob,
        b_prob: r.b_prob,
        z_prob: r.z_prob,
    }
}

/// Reads the regions from a native (headless) game.
pub fn read_view(game: &SM64Game) -> Result<View> {
    let mut bytes = vec![0u8; regions_len(&HEADLESS)];
    let mut o = 0;
    for (addr, len) in regions(&HEADLESS) {
        game.read_memory(addr, &mut bytes[o..o + len as usize])?;
        o += len as usize;
    }
    Ok(view(&bytes))
}

/// A game with its perturbation: what a miner plays and a verifier replays.
/// Copy both together (or [`MiningGame::save`] / [`MiningGame::load`]) to
/// branch a search: the perturbation state is part of the game's state.
pub struct MiningGame {
    pub game: SM64Game,
    pub pert: Perturber,
}

/// A save state of a [`MiningGame`].
#[derive(Clone)]
pub struct MiningSave {
    pub game: SaveState,
    pub pert: Perturber,
}

impl MiningGame {
    /// A fresh game, powered on, with the block's perturbation armed.
    pub fn new(seed: &Seed, p: &Params) -> Result<MiningGame> {
        Ok(MiningGame { game: crate::generator()?.create_game()?, pert: Perturber::new(&seed.0, &poke_config(&p.rng)) })
    }

    /// Plays one frame: the perturbation may replace `wanted` with a random
    /// pad (a poke). Returns the pad played and whether it was a poke.
    pub fn step(&mut self, wanted: GamePad) -> Result<(GamePad, bool)> {
        let before = read_view(&self.game)?.clock;
        let (played, poked) = self.pert.pad(before, to_pad(wanted));
        let played = from_pad(played);
        self.game.step_game(played)?;
        let after = read_view(&self.game)?;
        if after.clock != before {
            self.pert.mix(&after.digest);
        }
        Ok((played, poked))
    }

    pub fn state(&mut self) -> Result<GameState> {
        self.game.get_game_state()
    }

    /// A copy (a new game instance with the same memory and perturbation).
    pub fn fork(&self) -> Result<MiningGame> {
        Ok(MiningGame { game: crate::generator()?.copy_from(&self.game)?, pert: self.pert.clone() })
    }

    /// Becomes a copy of `base` (cheap when repeated from the same base).
    pub fn sync_from(&mut self, base: &MiningGame) -> Result<()> {
        self.game.sync_from(&base.game)?;
        self.pert = base.pert.clone();
        Ok(())
    }

    pub fn save(&self) -> MiningSave {
        MiningSave { game: self.game.save_state(), pert: self.pert.clone() }
    }

    pub fn load(&mut self, s: &MiningSave) -> Result<()> {
        self.game.load_state(&s.game)?;
        self.pert = s.pert.clone();
        Ok(())
    }
}

