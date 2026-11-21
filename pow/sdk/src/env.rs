//! [`Task`] and [`MiningEnv`]: the contract between a bot and the chain.

use crate::daemon::Template;
use anyhow::{Result, bail};
use rand::{RngCore, SeedableRng, rngs::StdRng};
use sm64_binds::{GamePad, GameState, SM64Game};
use sm64pow::{MiningGame, MiningSave, Params, Seed, Verdict};

/// What one block asks of a miner: the seed hash and the difficulty (which
/// fixes the time limit and the perturbation strength).
#[derive(Clone, Debug)]
pub struct Task {
    pub seed: Seed,
    pub difficulty: u64,
    pub params: Params,
    /// Set for daemon tasks.
    pub template: Option<Template>,
}

impl Task {
    /// A task from explicit consensus inputs (seed hash and difficulty).
    pub fn new(seed: Seed, difficulty: u64) -> Task {
        Task { seed, difficulty, params: sm64pow::params(difficulty), template: None }
    }

    /// A local practice task: the seed hash is derived from `seed` (any
    /// number), the difficulty is yours to pick (1 is the easiest).
    pub fn practice(seed: u64, difficulty: u64) -> Task {
        let mut h = [0u8; 32];
        StdRng::seed_from_u64(seed).fill_bytes(&mut h);
        Task::new(Seed(h), difficulty)
    }

    /// A task from a daemon block template. Fails if this build's rules
    /// (`sm64pow`) disagree with the daemon's: rebuild against the same
    /// sm64pow as the daemon.
    pub fn from_template(t: &Template) -> Result<Task> {
        let seed = Seed::from_hex(&t.sm64_seed_hash)?;
        let params = sm64pow::params(t.difficulty);
        if params.max_frames != t.sm64_max_frames
            || params.rng.random_amount != t.sm64_random_amount
            || params.reseed_frames != t.sm64_reseed_frames
        {
            bail!(
                "this SDK's rules ({}: {} frames, {} pokes) differ from the daemon's ({}: {} frames, {} pokes): rebuild with the daemon's sm64pow",
                sm64pow::rules().name(),
                params.max_frames,
                params.rng.random_amount,
                t.sm64_rules,
                t.sm64_max_frames,
                t.sm64_random_amount
            );
        }
        Ok(Task { seed, difficulty: t.difficulty, params, template: Some(t.clone()) })
    }
}

/// A saved point of a run (game memory plus the frames played so far).
pub struct Snapshot {
    state: MiningSave,
    frame: usize,
    success: bool,
}

/// A game being played for a [`Task`], with the consensus perturbation applied.
///
/// [`MiningEnv::step`] is the only way to advance a committed run: it passes
/// the wanted pad through the perturbation (generated outside the game from the
/// seed, scheduled on the game's clock), steps the game with the pad that came
/// out and records it. The proof keeps only the non-poked frames' inputs.
///
/// Exploring futures: [`MiningEnv::fork`] (a copy, to simulate on) and
/// [`MiningEnv::save`] / [`MiningEnv::restore`] (rewind) go through the very
/// same perturbation, so what you simulate is what would be played.
pub struct MiningEnv {
    game: MiningGame,
    task: Task,
    pads: Vec<GamePad>,
    success: bool,
    state: Option<GameState>,
}

impl MiningEnv {
    /// A powered-on game with the perturbation armed. The ROM must be loaded
    /// ([`crate::init_rom`]).
    pub fn new(task: &Task) -> Result<MiningEnv> {
        let game = sm64pow::new_mining_game(&task.seed, &task.params)?;
        Ok(MiningEnv { game, task: task.clone(), pads: Vec::new(), success: false, state: None })
    }

    pub fn task(&self) -> &Task {
        &self.task
    }

    /// Plays one frame. `wanted` is what your bot wants to press; the result
    /// is what was actually played (differs on poked frames). Errors once the
    /// run is over: success already reached, or the frame limit hit.
    pub fn step(&mut self, wanted: GamePad) -> Result<GamePad> {
        if self.success {
            bail!("the run already reached the success condition at frame {}; no more frames may be played", self.pads.len());
        }
        if self.is_over() {
            bail!("frame limit of {} reached", self.task.params.max_frames);
        }
        let played = sm64pow::mine_step(&mut self.game, wanted)?;
        self.pads.push(played);
        let st = self.game.state()?;
        self.success = sm64pow::rules().is_success(&self.game.game, &st);
        self.state = Some(st);
        Ok(played)
    }

    /// Frames played so far (the next frame's 0-based index).
    pub fn frame(&self) -> usize {
        self.pads.len()
    }

    pub fn max_frames(&self) -> usize {
        self.task.params.max_frames as usize
    }

    pub fn frames_left(&self) -> usize {
        self.max_frames().saturating_sub(self.pads.len())
    }

    /// Mario's and the level's state after the last frame (cached).
    pub fn state(&mut self) -> Result<GameState> {
        if self.state.is_none() {
            self.state = Some(self.game.state()?);
        }
        Ok(self.state.clone().unwrap())
    }

    /// The success condition holds now, on the last frame played.
    pub fn is_success(&self) -> bool {
        self.success
    }

    /// No more frames can be played (success reached or time limit hit).
    pub fn is_over(&self) -> bool {
        self.success || self.pads.len() >= self.max_frames()
    }

    /// The pads actually played so far.
    pub fn pads(&self) -> &[GamePad] {
        &self.pads
    }

    /// The underlying game, read-only use (memory reads, object tables, ...).
    /// Do not step it yourself: that breaks the contract.
    pub fn game_mut(&mut self) -> &mut SM64Game {
        &mut self.game.game
    }

    /// An independent copy of this run, to simulate futures on.
    pub fn fork(&self) -> Result<MiningEnv> {
        let game = self.game.fork()?;
        Ok(MiningEnv { game, task: self.task.clone(), pads: self.pads.clone(), success: self.success, state: self.state.clone() })
    }

    /// Makes this run a copy of `other` (cheaper than [`MiningEnv::fork`]: no
    /// new game). Both must be for the same task.
    pub fn copy_from(&mut self, other: &MiningEnv) -> Result<()> {
        self.game.sync_from(&other.game)?;
        self.pads.clear();
        self.pads.extend_from_slice(&other.pads);
        self.success = other.success;
        self.state = other.state.clone();
        Ok(())
    }

    pub fn save(&self) -> Snapshot {
        Snapshot { state: self.game.save(), frame: self.pads.len(), success: self.success }
    }

    /// Rewinds to `snap`, taken from this run's line (frames after it are
    /// dropped; `snap` must not be from further along than this run's pads).
    pub fn restore(&mut self, snap: &Snapshot) -> Result<()> {
        if snap.frame > self.pads.len() {
            bail!("snapshot at frame {} is ahead of this run ({}); it belongs to another line", snap.frame, self.pads.len());
        }
        self.game.load(&snap.state)?;
        self.pads.truncate(snap.frame);
        self.success = snap.success;
        self.state = None;
        Ok(())
    }

    /// The proof blob in the smallest codec. Meaningful once
    /// [`MiningEnv::is_success`].
    pub fn proof(&self) -> Vec<u8> {
        // Only the non-poked frames' inputs are stored; replays to find them.
        sm64pow::seal_proof(&self.task.seed, &self.task.params, &self.pads).expect("replaying own run")
    }

    /// Raw pads, 4 bytes a frame (u16 LE buttons, i8 x, i8 y): the format
    /// `examples/verify_file` takes.
    pub fn proof_raw(&self) -> Vec<u8> {
        self.pads.iter().flat_map(sm64pow::proof::pad_bytes).collect()
    }

    /// Runs the consensus verifier (a full replay) on the proof so far.
    pub fn verify(&self) -> Result<Verdict> {
        sm64pow::verify(&self.task.seed, self.task.difficulty, &self.proof())
    }
}
