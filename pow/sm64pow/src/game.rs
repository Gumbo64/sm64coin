//! The game adapter: what the perturbation reads from Super Mario 64, and a
//! game bundled with its perturbation ([`SM64MiningGame`]).
//!
//! The adapter names a few memory regions; native code reads them from the
//! game, the browser copies the same bytes out of its build's memory and
//! hands them to this code compiled to WASM. Both then compute the clock and
//! digest with [`view`]. Another game needs another adapter (its regions,
//! clock and digest) and nothing else.

use crate::{Params, Seed};
use anyhow::Result;
use crate::rules::RngConfig;
use sm64_binds::{GamePad, GameState, SM64Game, SaveState};
pub use sm64coin_perturb::sm64::{Addrs, Event, HEADLESS, Progress, VISUAL, event, View, progress, progress_regions, progress_regions_len, regions, regions_len, view};
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

/// How far into the game a native (headless) game is: stars, Bowser keys,
/// the Grand Star (see the rules).
pub fn read_progress(game: &SM64Game) -> Result<Progress> {
    let mut bytes = vec![0u8; progress_regions_len(&HEADLESS)];
    let mut o = 0;
    for (addr, len) in progress_regions(&HEADLESS) {
        game.read_memory(addr, &mut bytes[o..o + len as usize])?;
        o += len as usize;
    }
    Ok(progress(&bytes))
}

/// A game with its perturbation: what a miner plays and a verifier replays.
///
/// It is an [`SM64Game`] (it derefs to one, so every `SM64Game` method works
/// on it), except that the calls which play or branch the game are its own
/// versions under the same names, carrying the perturbation:
/// [`step_game`](SM64MiningGame::step_game) pokes, and
/// [`copy_from`](SM64MiningGame::copy_from), [`sync_from`](SM64MiningGame::sync_from),
/// [`save_state`](SM64MiningGame::save_state) / [`load_state`](SM64MiningGame::load_state)
/// include the perturbation state. Code written against `SM64Game` that
/// takes a `SM64MiningGame` instead runs perturbed. (Writing game memory directly
/// through the deref bypasses nothing the verifier checks, but such a run
/// won't replay.)
pub struct SM64MiningGame {
    pub game: SM64Game,
    pub pert: Perturber,
}

/// A save state of a [`SM64MiningGame`].
#[derive(Clone)]
pub struct SM64MiningSave {
    pub game: SaveState,
    pub pert: Perturber,
}

impl SM64MiningGame {
    /// A fresh game, powered on, with the block's perturbation armed.
    pub fn new(seed: &Seed, p: &Params) -> Result<SM64MiningGame> {
        Ok(SM64MiningGame { game: crate::generator()?.create_game()?, pert: Perturber::new(&seed.0, &poke_config(&p.rng)) })
    }

    /// `game` (in whatever state, e.g. the parent block's end state) with a
    /// block's perturbation armed from here on.
    pub fn from_game(game: SM64Game, seed: &Seed, p: &Params) -> SM64MiningGame {
        SM64MiningGame { game, pert: Perturber::new(&seed.0, &poke_config(&p.rng)) }
    }

    /// This game's progress (see [`read_progress`]).
    pub fn progress(&self) -> Result<Progress> {
        read_progress(&self.game)
    }

    /// Plays one frame, perturbed: `SM64Game::step_game` with the pokes.
    pub fn step_game(&mut self, wanted: GamePad) -> Result<()> {
        self.step(wanted).map(|_| ())
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
    pub fn fork(&self) -> Result<SM64MiningGame> {
        Ok(SM64MiningGame { game: crate::generator()?.copy_from(&self.game)?, pert: self.pert.clone() })
    }

    /// Becomes a copy of `source` (game and perturbation).
    pub fn copy_from(&mut self, source: &SM64MiningGame) -> Result<()> {
        self.game.copy_from(&source.game)?;
        self.pert = source.pert.clone();
        Ok(())
    }

    /// Becomes a copy of `base` (cheap when repeated from the same base).
    pub fn sync_from(&mut self, base: &SM64MiningGame) -> Result<()> {
        self.game.sync_from(&base.game)?;
        self.pert = base.pert.clone();
        Ok(())
    }

    pub fn save_state(&self) -> SM64MiningSave {
        SM64MiningSave { game: self.game.save_state(), pert: self.pert.clone() }
    }

    pub fn load_state(&mut self, s: &SM64MiningSave) -> Result<()> {
        self.game.load_state(&s.game)?;
        self.pert = s.pert.clone();
        Ok(())
    }
}

impl std::ops::Deref for SM64MiningGame {
    type Target = SM64Game;
    fn deref(&self) -> &SM64Game {
        &self.game
    }
}

impl std::ops::DerefMut for SM64MiningGame {
    fn deref_mut(&mut self) -> &mut SM64Game {
        &mut self.game
    }
}
