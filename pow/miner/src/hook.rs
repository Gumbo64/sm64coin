//! The SM64Coin perturbation as obrrt-search's step hook: the search's
//! workers play every frame through [`sm64pow::game::perturbed_step`], the
//! same function the verifier's [`SM64MiningGame`] steps with.

use anyhow::{Result, anyhow};
use obrrt_search::game::{HookedGame, HookedSave, StepHook};
use sm64_binds::{GamePad, SM64Game};
use sm64pow::game::{Perturber, SM64MiningGame, SM64MiningSave, perturbed_step};
use std::any::Any;

/// A block's perturbation, as a step hook.
#[derive(Clone)]
pub struct PertHook(pub Perturber);

impl StepHook for PertHook {
    fn step(&mut self, game: &mut SM64Game, wanted: GamePad) -> Result<GamePad> {
        Ok(perturbed_step(game, &mut self.0, wanted)?.0)
    }
    fn clone_box(&self) -> Box<dyn StepHook> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A search game: a fork of `g`, perturbation included.
pub fn hooked(g: &SM64MiningGame) -> Result<HookedGame> {
    let generator = sm64pow::generator()?;
    Ok(HookedGame::new(generator.copy_from(&g.game)?, Box::new(PertHook(g.pert.clone())), generator))
}

/// A mining save as the search's save.
pub fn hooked_save(s: &SM64MiningSave) -> HookedSave {
    HookedSave { game: s.game.clone(), hook: Box::new(PertHook(s.pert.clone())) }
}

/// The search's save as a mining save (its hook must be a [`PertHook`]).
pub fn mining_save(s: &HookedSave) -> Result<SM64MiningSave> {
    let p = s.hook.as_any().downcast_ref::<PertHook>().ok_or_else(|| anyhow!("search save without the SM64Coin perturbation"))?;
    Ok(SM64MiningSave { game: s.game.clone(), pert: p.0.clone() })
}
