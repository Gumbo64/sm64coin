//! [`Task`] and [`MiningEnv`]: the contract between a bot and the chain.

use crate::daemon::{Daemon, Template};
use anyhow::{Result, bail};
use rand::{RngCore, SeedableRng, rngs::StdRng};
use sm64_binds::{GamePad, GameState, SM64Game, SaveState};
use sm64pow::game::Progress;
use sm64pow::proof::Start;
use sm64pow::{SM64MiningGame, SM64MiningSave, Params, Seed, Verdict};
use std::sync::Arc;

/// What one block asks of a miner: where the run starts (power-on, or the
/// parent block's end state), the seed hash, and the time limit.
///
/// Success: the game makes its next progress point (a star, a Bowser key, a cap switch, or
/// the Grand Star with 70 stars) compared with the start's progress.
#[derive(Clone)]
pub struct Task {
    pub seed: Seed,
    /// The retarget difficulty (only the time limit depends on it).
    pub difficulty: u64,
    pub params: Params,
    /// Where the run starts. A [`Start::Parent`] task carries the parent's end state.
    pub start: Start,
    pub parent: Option<Arc<SaveState>>,
    /// The progress the run starts with: success is one point more.
    pub start_progress: Progress,
    /// The time limit when the template was made (frames). It grows by the
    /// second after the grace period; see [`Task::frame_cap`].
    pub max_frames: u32,
    /// The most frames [`MiningEnv`] lets a run play. A run longer than the
    /// limit is not lost: it becomes valid when the limit has grown to it
    /// ([`Daemon::template_valid_at`]); [`run_miner`](crate::run_miner) waits for that.
    pub frame_cap: u32,
    /// Seconds the template's timestamp is after the parent's (the decay is
    /// computed from it): 0 for local tasks.
    pub elapsed: u64,
    /// Set for daemon tasks.
    pub template: Option<Template>,
}

impl std::fmt::Debug for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task").field("difficulty", &self.difficulty).field("start", &self.start).field("start_progress", &self.start_progress).field("max_frames", &self.max_frames).field("frame_cap", &self.frame_cap).finish()
    }
}

impl Task {
    /// A power-on task from explicit consensus inputs (seed hash and difficulty).
    pub fn new(seed: Seed, difficulty: u64) -> Task {
        // Offline (no parent block): the difficulty's limit once grown, as on a parent long
        // past (the limit starts lower and grows, see rules.rs start_halvings).
        let max = sm64pow::params(difficulty.max(1)).max_frames;
        Task {
            seed,
            difficulty,
            params: sm64pow::params(difficulty),
            start: Start::PowerOn,
            parent: None,
            start_progress: Progress::default(),
            max_frames: max,
            frame_cap: max,
            elapsed: 0,
            template: None,
        }
    }

    /// A local practice task from power-on: the seed hash is derived from
    /// `seed` (any number), the difficulty is yours to pick (1 is the easiest).
    pub fn practice(seed: u64, difficulty: u64) -> Task {
        Task::new(practice_seed(seed), difficulty)
    }

    /// A local task that continues from `parent` (the end state of a finished
    /// run, [`MiningEnv::end_state`]), for testing the next block of a cycle.
    pub fn after(parent: SaveState, progress: Progress, seed: Seed, difficulty: u64) -> Task {
        Task { start: Start::Parent, parent: Some(Arc::new(parent)), start_progress: progress, ..Task::new(seed, difficulty) }
    }

    /// A task from a daemon block template, fetching the current cycle to
    /// rebuild the parent's end state when the run starts from the parent
    /// (`sm64_start` is "parent" or "either"; "either" prefers the parent). A
    /// power-on start needs no state. Fails if this build's rules (`sm64pow`)
    /// disagree with the daemon's: rebuild against the same sm64pow.
    pub fn from_template(t: &Template, daemon: &Daemon) -> Result<Task> {
        let start = if t.sm64_start == "power-on" { Start::PowerOn } else { Start::Parent };
        Task::from_template_at(t, daemon, start)
    }

    /// [`Task::from_template`] with a chosen start (power-on is only allowed
    /// when the template's `sm64_start` is "power-on" or "either").
    pub fn from_template_at(t: &Template, daemon: &Daemon, start: Start) -> Result<Task> {
        let seed = Seed::from_hex(&t.sm64_seed_hash)?;
        let retarget = t.sm64_retarget_difficulty.max(1);
        let params = sm64pow::params(retarget);
        let elapsed = t.sm64_timestamp.saturating_sub(t.sm64_parent_timestamp);
        let limit = sm64pow::rules().max_frames(retarget, elapsed);
        if (!t.sm64_rules.is_empty() && t.sm64_rules != sm64pow::rules().name()) || limit != t.sm64_max_frames || params.rng.random_amount != t.sm64_random_amount || params.reseed_frames != t.sm64_reseed_frames {
            bail!(
                "this SDK's rules ({}: {} frames, {} pokes) differ from the daemon's ({}: {} frames, {} pokes): rebuild with the daemon's sm64pow",
                sm64pow::rules().name(),
                limit,
                params.rng.random_amount,
                t.sm64_rules,
                t.sm64_max_frames,
                t.sm64_random_amount
            );
        }
        if start == Start::PowerOn && t.sm64_start == "parent" {
            bail!("this block must continue from its parent (power-on is allowed from {})", t.sm64_power_on_at);
        }
        if start == Start::Parent && t.sm64_start == "power-on" {
            bail!("this block must start from power-on");
        }
        let (parent, start_progress) = match start {
            Start::PowerOn => (None, Progress::default()),
            Start::Parent => {
                let cycle = daemon.cycle(Some(&t.prev_hash))?;
                let (state, p) = crate::daemon::cycle_end_state(&cycle)?;
                (Some(Arc::new(state)), p)
            }
        };
        Ok(Task {
            seed,
            difficulty: retarget,
            params,
            start,
            parent,
            start_progress,
            max_frames: limit,
            frame_cap: limit,
            elapsed,
            template: Some(t.clone()),
        })
    }

    /// Lets a run play on past the current time limit, up to the most any
    /// difficulty allows (see [`Task::frame_cap`]).
    pub fn allow_over_budget(mut self) -> Task {
        self.frame_cap = sm64pow::rules().max_frames_any();
        self
    }

    /// A fresh game at this task's start with the block's perturbation armed.
    pub fn start_game(&self) -> Result<SM64MiningGame> {
        Ok(sm64pow::chain::mining_start(self.start, self.parent.as_deref(), &self.seed, &self.params)?.0)
    }

    /// Replays the proof `blob` the way the verifier does (without the time
    /// limit, which depends on the block's timestamp): the success condition
    /// must first hold on exactly the last frame and every input be used.
    pub fn check(&self, blob: &[u8]) -> Result<Verdict> {
        // Any codec, the committed (v0.4) or v0.3 layout as the active features say; state-aware
        // codecs are decoded by replaying the run; a committed blob must decode to its input hash.
        let pr = match sm64pow::decode_proof_any(blob, self.parent.as_deref(), &self.seed, &self.params) {
            Ok(p) => p,
            Err(e) => return Ok(Verdict::Invalid(format!("bad proof: {e:#}"))),
        };
        if pr.start != self.start {
            return Ok(Verdict::Invalid(format!("proof starts from {:?}, the task from {:?}", pr.start, self.start)));
        }
        if pr.frames == 0 || pr.inputs.len() > pr.frames as usize {
            return Ok(Verdict::Invalid("empty proof or more inputs than frames".into()));
        }
        let mut g = self.start_game()?;
        let mut next = 0usize;
        for f in 0..pr.frames as usize {
            let wanted = pr.inputs.get(next).copied().unwrap_or_default();
            let (_, poked) = g.step(wanted)?;
            if !poked {
                if next >= pr.inputs.len() {
                    return Ok(Verdict::Invalid(format!("frame {f} needs an input but the proof has run out")));
                }
                next += 1;
            }
            if sm64pow::rules().is_success(&self.start_progress, &g.progress()?) {
                if f + 1 != pr.frames as usize {
                    return Ok(Verdict::Invalid(format!("success at frame {} but the proof runs to {}", f + 1, pr.frames)));
                }
                if next != pr.inputs.len() {
                    return Ok(Verdict::Invalid(format!("{} inputs left over", pr.inputs.len() - next)));
                }
                return Ok(Verdict::Valid { frames: pr.frames });
            }
        }
        Ok(Verdict::Invalid("no progress was made (no new star, key, cap switch or Grand Star)".into()))
    }
}

pub(crate) fn practice_seed(seed: u64) -> Seed {
    let mut h = [0u8; 32];
    StdRng::seed_from_u64(seed).fill_bytes(&mut h);
    Seed(h)
}

/// A saved point of a run (game memory plus the frames played so far).
pub struct Snapshot {
    state: SM64MiningSave,
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
    game: SM64MiningGame,
    task: Task,
    pads: Vec<GamePad>,
    success: bool,
    state: Option<GameState>,
}

impl MiningEnv {
    /// A game at the task's start (powered on, or the parent's end state) with
    /// the perturbation armed. The ROM must be loaded ([`crate::init_rom`]).
    pub fn new(task: &Task) -> Result<MiningEnv> {
        let game = task.start_game()?;
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
            bail!("frame limit of {} reached", self.task.frame_cap);
        }
        let played = sm64pow::mine_step(&mut self.game, wanted)?;
        self.pads.push(played);
        self.success = sm64pow::rules().is_success(&self.task.start_progress, &self.game.progress()?);
        self.state = None;
        Ok(played)
    }

    /// Frames played so far (the next frame's 0-based index).
    pub fn frame(&self) -> usize {
        self.pads.len()
    }

    /// The time limit when the task was made. A longer run is valid later
    /// (the limit grows by the second after a grace period).
    pub fn max_frames(&self) -> usize {
        self.task.max_frames as usize
    }

    /// The most frames this run may play (the limit, or more with [`Task::allow_over_budget`]).
    pub fn frame_cap(&self) -> usize {
        self.task.frame_cap as usize
    }

    pub fn frames_left(&self) -> usize {
        self.frame_cap().saturating_sub(self.pads.len())
    }

    /// Mario's and the level's state after the last frame (cached).
    pub fn state(&mut self) -> Result<GameState> {
        if self.state.is_none() {
            self.state = Some(self.game.state()?);
        }
        Ok(self.state.clone().unwrap())
    }

    /// The game's progress now (stars, keys, the Grand Star).
    pub fn progress(&self) -> Result<Progress> {
        self.game.progress()
    }

    /// The game's state after the last frame, as a save state: the parent
    /// end state of the next block ([`Task::after`]). Meaningful on success.
    pub fn end_state(&self) -> SaveState {
        self.game.game.save_state()
    }

    /// The success condition holds now, on the last frame played: one more
    /// star, Bowser key, cap switch or the Grand Star than the run started with.
    pub fn is_success(&self) -> bool {
        self.success
    }

    /// No more frames can be played (success reached or the frame cap hit).
    pub fn is_over(&self) -> bool {
        self.success || self.pads.len() >= self.frame_cap()
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
        Snapshot { state: self.game.save_state(), frame: self.pads.len(), success: self.success }
    }

    /// Rewinds to `snap`, taken from this run's line (frames after it are
    /// dropped; `snap` must not be from further along than this run's pads).
    pub fn restore(&mut self, snap: &Snapshot) -> Result<()> {
        if snap.frame > self.pads.len() {
            bail!("snapshot at frame {} is ahead of this run ({}); it belongs to another line", snap.frame, self.pads.len());
        }
        self.game.load_state(&snap.state)?;
        self.pads.truncate(snap.frame);
        self.success = snap.success;
        self.state = None;
        Ok(())
    }

    /// The proof blob in the smallest codec, after the input normaliser (sm64pow::normalise: pads
    /// that don't change what the game does are replaced by cheaper ones, ~12-21 % smaller proofs;
    /// every candidate is replay-checked, the original run is the fallback). It costs seconds on a
    /// long run: [`MiningEnv::proof_unnormalised`] skips it. Meaningful once
    /// [`MiningEnv::is_success`].
    pub fn proof(&self) -> Vec<u8> {
        sm64pow::normalise::seal_proof_normalised_at(self.task.start, self.task.parent.as_deref(), &self.task.seed, &self.task.params, &self.pads).expect("replaying own run")
    }

    /// The proof blob after the replay-free exact canonicaliser (sm64pow::exact: exact stick classes and
    /// unread buttons from the input analysis; ~0.1-1 s, ~5 % smaller proofs; replay-checked, the original
    /// run is the fallback). [`MiningEnv::proof`] saves more (~20 %) but takes seconds to a minute.
    pub fn proof_exact(&self) -> Vec<u8> {
        use sm64pow::normalise::{Method, StickPick, seal_proof_exact_report};
        seal_proof_exact_report(self.task.start, self.task.parent.as_deref(), &self.task.seed, &self.task.params, &self.pads, Method::Exact, StickPick::Model).expect("replaying own run").0
    }

    /// [`MiningEnv::proof`] without the normaliser: the run exactly as played.
    pub fn proof_unnormalised(&self) -> Vec<u8> {
        // Only the non-poked frames' inputs are stored; replays to find them.
        sm64pow::seal_proof_at(self.task.start, self.task.parent.as_deref(), &self.task.seed, &self.task.params, &self.pads).expect("replaying own run")
    }

    /// Raw pads, 4 bytes a frame (u16 LE buttons, i8 x, i8 y): the format
    /// `examples/verify_file` takes.
    pub fn proof_raw(&self) -> Vec<u8> {
        self.pads.iter().flat_map(sm64pow::proof::pad_bytes).collect()
    }

    /// Replays the proof so far the way the verifier does ([`Task::check`]).
    pub fn verify(&self) -> Result<Verdict> {
        self.task.check(&self.proof())
    }
}
