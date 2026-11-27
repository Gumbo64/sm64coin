//! The proof of play: replaying blocks' Mario runs with sm64pow's verifier, the same code the node
//! uses (`chain::verify_any` / `rebuild_any`). Feature `replay`; needs the game ROM at runtime
//! ([`Sm64Replay::init`] or `$SM64COIN_ROM`).
//!
//! Header checks cannot tell whether a committed input hash is a real run, so this layer is what turns
//! a header-valid chain into a valid one: every replayed block must play its committed inputs under the
//! block's own perturbation from its parent's end state and reach its progress point exactly on its last
//! frame within the time limit. Cost: a cycle (up to ~70 blocks) is seconds to a minute natively
//! (measured: see `tests/replay.rs` output), longer in WASM. It is synchronous and CPU bound: run it on a
//! worker thread; it checks the cancel flag and the budget between blocks.
//!
//! Scopes: [`Sm64Replay`] as a [`Judge`] replays the current cycle of a [`Branch`] (plus `extra_cycles`
//! earlier ones kept in it). [`Sm64Replay::replay_history`] replays blocks fetched with
//! [`crate::verify::backfill`] (history between two checkpoints), for the one-random-old-cycle spot check.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sm64pow::Seed;
use sm64pow::chain::{self, BlockVerdict, CommittedVerdict, Parent};

use crate::block::{Block, Hash};
use crate::verify::{Branch, Judge, JudgeVerdict};

/// How much replay to do, and how to stop it.
#[derive(Clone, Default)]
pub struct ReplayBudget {
    /// Earlier cycles replayed besides the current one (0: current only, the Android default).
    pub extra_cycles: usize,
    /// Stop after this many blocks (the result is [`JudgeVerdict::Partial`]).
    pub max_blocks: Option<usize>,
    /// Stop after this long (checked between blocks; ignored on wasm32, which has no clock here).
    pub max_duration: Option<Duration>,
    /// Set from another thread to stop (a background task being cancelled).
    pub cancel: Option<Arc<AtomicBool>>,
}

#[derive(Clone, Copy, Debug)]
pub struct ReplayProgress {
    pub done: usize,
    pub total: usize,
    pub height: u64,
}

pub type ProgressFn = Arc<dyn Fn(&ReplayProgress) + Send + Sync>;

/// [`Judge`] with sm64pow.
#[derive(Clone, Default)]
pub struct Sm64Replay {
    pub budget: ReplayBudget,
    pub progress: Option<ProgressFn>,
}

struct Item {
    id: Hash,
    height: u64,
    seed: Seed,
    proof: Vec<u8>,
    /// Some: the run is also held to the time limit at this difficulty (current chain, header-verified).
    difficulty: Option<u64>,
    timestamp: u64,
    frames: u32,
}

impl Sm64Replay {
    pub fn new(budget: ReplayBudget) -> Self {
        Self { budget, progress: None }
    }

    /// Loads the game ROM (the SM64 US ROM bytes). Without it every replay answers `Unavailable`.
    pub fn init(rom: Vec<u8>) -> Result<(), String> {
        sm64pow::init(rom).map(|_| ()).map_err(|e| format!("{e:#}"))
    }

    /// Whether replay can run here (the ROM is loaded or found at `$SM64COIN_ROM` / `baserom.us.z64`).
    pub fn available() -> Result<(), String> {
        sm64pow::generator().map(|_| ()).map_err(|e| format!("{e:#}"))
    }

    fn stop(&self, started: &Instant0) -> Option<String> {
        if self.budget.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Some("cancelled".into());
        }
        if let Some(max) = self.budget.max_duration {
            if started.elapsed() >= max {
                return Some("time budget used up".into());
            }
        }
        None
    }

    /// Replays `items` in order, the first from power-on (as a cycle's first block).
    fn run(&self, items: Vec<Item>) -> JudgeVerdict {
        if items.is_empty() {
            return JudgeVerdict::Valid { replayed: 0 };
        }
        if let Err(e) = Self::available() {
            return JudgeVerdict::Unavailable(e);
        }
        let started = Instant0::now();
        let total = items.len();
        let mut prev: Option<(Hash, u64)> = None;
        for (n, it) in items.iter().enumerate() {
            if let Some(why) = self.stop(&started) {
                return JudgeVerdict::Partial { replayed: n, reason: why };
            }
            if self.budget.max_blocks.is_some_and(|m| n >= m) {
                return JudgeVerdict::Partial { replayed: n, reason: "block budget used up".into() };
            }
            let parent = match prev {
                // a cycle's first block: power-on, whatever came before it
                None => Parent::Genesis,
                Some((id, ts)) => Parent::Block { id, gap_seconds: it.timestamp.saturating_sub(ts) },
            };
            let v = match it.difficulty {
                Some(d) => chain::verify_any(&it.id, parent, &it.seed, d, &it.proof),
                None => chain::rebuild_any(&it.id, parent, &it.seed, &it.proof),
            };
            let bad = |why: String| JudgeVerdict::Invalid { height: it.height, reason: why };
            match v {
                Err(e) => return JudgeVerdict::Unavailable(format!("replay failed to run at block {}: {e:#}", it.height)),
                Ok(CommittedVerdict::Run(BlockVerdict::Valid { frames, .. })) => {
                    if frames != it.frames {
                        return bad(format!("replays {frames} frames, the header says {}", it.frames));
                    }
                }
                Ok(CommittedVerdict::Run(BlockVerdict::Invalid(why))) => return bad(why),
                Ok(CommittedVerdict::Run(BlockVerdict::NeedParent)) => return JudgeVerdict::Partial { replayed: n, reason: "the parent's end state was evicted".into() },
                Ok(CommittedVerdict::BadEncoding(why)) => return bad(format!("the proof's encoding does not decode to its committed inputs: {why}")),
            }
            prev = Some((it.id, it.timestamp));
            if let Some(p) = &self.progress {
                p(&ReplayProgress { done: n + 1, total, height: it.height });
            }
        }
        JudgeVerdict::Valid { replayed: total }
    }

    /// Replays blocks of history (from [`crate::verify::backfill`]): from the first power-on start in
    /// `blocks`, no time limits (those were the header layer's business at the time). Replays whole
    /// cycles only: a trailing cycle that is cut off is replayed as far as it goes.
    pub fn replay_history(&self, blocks: &[Block]) -> JudgeVerdict {
        let Some(first) = blocks.iter().position(|b| b.proof_header().is_some_and(|h| h.power_on)) else {
            return JudgeVerdict::Unavailable("no cycle start in these blocks".into());
        };
        self.run(blocks[first..].iter().map(item_of_block).collect())
    }
}

fn item_of_block(b: &Block) -> Item {
    Item {
        id: b.id(),
        height: b.height,
        seed: Seed(b.seed()),
        proof: b.proof.clone(),
        difficulty: None,
        timestamp: b.header.timestamp,
        frames: b.proof_header().map_or(0, |h| h.frames),
    }
}

impl Judge for Sm64Replay {
    fn judge(&self, branch: &Branch) -> JudgeVerdict {
        if branch.raw.is_empty() {
            return JudgeVerdict::Valid { replayed: 0 };
        }
        let (at, complete) = branch.cycle_start(self.budget.extra_cycles);
        if !complete {
            return JudgeVerdict::Unavailable("the verified blocks kept do not reach back to the start of the cycle (raise VerifyOptions::keep_blocks)".into());
        }
        let off = branch.blocks.len() - branch.raw.len();
        let items = branch.raw[at..]
            .iter()
            .zip(&branch.blocks[off + at..])
            .map(|(b, v)| Item { difficulty: Some(v.difficulty), ..item_of_block(b) })
            .collect();
        self.run(items)
    }
}

/// `std::time::Instant` where it works, a no-op clock on wasm32.
struct Instant0(#[cfg(not(target_arch = "wasm32"))] std::time::Instant);
impl Instant0 {
    fn now() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        return Instant0(std::time::Instant::now());
        #[cfg(target_arch = "wasm32")]
        return Instant0();
    }
    fn elapsed(&self) -> Duration {
        #[cfg(not(target_arch = "wasm32"))]
        return self.0.elapsed();
        #[cfg(target_arch = "wasm32")]
        return Duration::ZERO;
    }
}
