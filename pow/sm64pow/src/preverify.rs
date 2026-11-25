//! The parallel pre-pass of a sync batch.

use crate::cache::{REPLAYS, Summary, alias, cached, known_summary, parent_state, remember, remember_summary, run_key};
use crate::emulator::generator;
use crate::game::{Progress, read_progress};
use crate::proof::{self, Start};
use crate::replay::{EndState, Parent, replay_blob, start_game};
use crate::rules::active as rules;
use crate::seed::Seed;
use anyhow::Result;
use sm64_binds::SaveState;

/// One block of a batch for [`preverify`].
pub struct BatchBlock {
    pub id: [u8; 32],
    pub parent: Parent,
    pub seed: Seed,
    pub blob: Vec<u8>,
}

/// The parallel pre-pass of a batch of blocks in chain order (a sync batch):
/// it splits the batch into chains that can be replayed on their own (from a
/// power-on start, the genesis block, or a parent whose end state is cached)
/// and replays the chains on up to `threads` threads at once, keeping a
/// [`Summary`] of every run and the full end state of each chain's last block.
/// [`verify_block`] then finds each run and only re-checks the rules that
/// depend on the timestamp and the parent, so a batch spanning several cycles
/// verifies in parallel. Nothing here decides validity: an invalid run is just
/// not summarized (and the chain stops there), and the sequential check
/// replays and rejects it as before. Returns how many runs it replayed.
pub fn preverify(batch: &[BatchBlock], threads: usize) -> usize {
    let state_ok = crate::statecodec::state_codec_enabled();
    // Chains: each block continues the previous one when it starts from its end state.
    let mut chains: Vec<Vec<&BatchBlock>> = Vec::new();
    for b in batch {
        let start = match proof::proof_start(&b.blob) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let parent_id = match b.parent {
            Parent::Block { id, .. } => Some(id),
            Parent::Genesis => None,
        };
        let continues = start == Start::Parent && chains.last().and_then(|c| c.last()).is_some_and(|l| Some(l.id) == parent_id);
        if continues {
            chains.last_mut().unwrap().push(b);
        } else if start == Start::PowerOn || parent_id.is_none() || parent_id.is_some_and(|p| cached(&p).is_some()) {
            chains.push(vec![b]);
        } else if start == Start::Parent {
            // Not startable here (no cached parent): the sequential check rebuilds it.
            chains.push(Vec::new());
        }
    }
    chains.retain(|c| !c.is_empty());
    let replayed = std::sync::atomic::AtomicUsize::new(0);
    let next = std::sync::atomic::AtomicUsize::new(0);
    // The browser has no threads (spawning panics): replay the chains one after another.
    #[cfg(target_arch = "wasm32")]
    {
        let _ = threads;
        while let Some(chain) = chains.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
            let _ = replay_chain(chain, state_ok, &replayed);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::scope(|sc| {
        for _ in 0..threads.max(1).min(chains.len()) {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(chain) = chains.get(i) else { break };
                let _ = replay_chain(chain, state_ok, &replayed);
            });
        }
    });
    replayed.into_inner()
}

fn replay_chain(chain: &[&BatchBlock], state_ok: bool, replayed: &std::sync::atomic::AtomicUsize) -> Result<()> {
    let mut prev: Option<(SaveState, Progress, bool)> = None;
    for (k, b) in chain.iter().enumerate() {
        let start = proof::proof_start(&b.blob)?;
        let key = run_key(&b.parent, start, &b.seed, &b.blob);
        let last = k + 1 == chain.len();
        if let Some(sum) = known_summary(&key) {
            // Already known: continue from its state if this chain needs it.
            if last {
                alias(b.id, key);
                return Ok(());
            }
            match cached(&b.id).or_else(|| { alias(b.id, key); cached(&b.id) }) {
                Some(s) => {
                    prev = Some((s.game.clone(), s.progress, s.ends_cycle));
                    continue;
                }
                None => {
                    let _ = sum;
                    // Its state is gone: replay it below like any other.
                }
            }
        }
        let (game, before) = match (start, &prev) {
            (Start::PowerOn, _) => {
                let g = generator()?.create_game()?;
                let p = read_progress(&g)?;
                (g, p)
            }
            (Start::Parent, Some((st, p, ends))) => {
                if *ends {
                    return Ok(());
                }
                let mut g = generator()?.create_game()?;
                g.load_state(st)?;
                (g, *p)
            }
            (Start::Parent, None) => match start_game(start, &b.parent, parent_state(&b.parent), false)? {
                Some(Ok((g, p, _))) => (g, p),
                _ => return Ok(()),
            },
        };
        replayed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        REPLAYS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (g, now, frames) = match replay_blob(game, before, &b.seed, &b.blob, state_ok)? {
            Ok(x) => x,
            Err(_) => return Ok(()), // the sequential check rejects it
        };
        let ends_cycle = rules().ends_cycle(&now);
        let event = crate::game::event(&before, &now);
        let state = g.game.save_state();
        if last {
            remember(b.id, key, EndState { game: state, progress: now, ends_cycle, event }, frames, start, true);
        } else {
            remember_summary(b.id, key, Summary { frames, start, progress: now, ends_cycle, event });
            prev = Some((state, now, ends_cycle));
        }
    }
    Ok(())
}
