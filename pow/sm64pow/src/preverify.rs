//! The parallel pre-pass of a sync batch (layer 5).
//!
//! When the daemon receives a batch of blocks it first replays their runs in
//! parallel, chain by chain (a chain starts at a power-on run, the genesis
//! block, or a block whose parent's end state is cached), and keeps a
//! summary of each. The sequential check ([`crate::chain::verify_block`])
//! then finds each run and only re-checks the rules that depend on the
//! timestamp and the parent. Nothing here decides validity: an invalid run is
//! just not summarized, and the sequential check replays and rejects it.
//!
//! It runs before any rule has looked at the batch, so it bounds its own
//! work (docs/DOS.md): no run is replayed past the longest limit any block
//! can have, and once a block is found invalid no block after it in the batch
//! is replayed (the sequential check rejects that block and drops the rest of
//! the batch). A batch of fake blocks costs about one replay per thread.
//!
//! Read next: chain.rs.

use crate::cache::{REPLAYS, Summary, alias, cached, known_invalid, known_summary, parent_state, remember, remember_invalid, remember_summary, run_key};
use crate::emulator::generator;
use crate::game::{Progress, read_progress};
use crate::proof::{self, Start};
use crate::replay::{EndState, Parent, replay_blob_committed, start_game};
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
/// `Summary` of every run and the full end state of each chain's last block.
/// [`crate::chain::verify_block`] then finds each run and only re-checks the rules that
/// depend on the timestamp and the parent, so a batch spanning several cycles
/// verifies in parallel. Nothing here decides validity: an invalid run is just
/// not summarized (and the chain stops there), and the sequential check
/// replays and rejects it as before. Returns how many runs it replayed.
pub fn preverify(batch: &[BatchBlock], threads: usize) -> usize {
    let state_ok = crate::statecodec::state_codec_enabled();
    // v0.4: the batch carries committed blobs (commit.rs). Each is made ready for the layout
    // machinery the way the sequential check does it (chain::prepare: same layout, same cache
    // key), so the check finds the pre-pass's runs. A blob that is invalid by its header or a bad
    // encoding ends the pre-pass there (the sequential check decides).
    let ceiling0 = rules().max_frames_any();
    let mut prepared = Vec::with_capacity(batch.len());
    for b in batch {
        match crate::chain::prepare(&b.blob, ceiling0, state_ok) {
            Ok(p) => prepared.push(p),
            Err(_) => break,
        }
    }
    let converted: Vec<BatchBlock> = batch.iter().zip(&prepared).map(|(b, p)| BatchBlock { id: b.id, parent: b.parent, seed: b.seed, blob: p.layout.clone() }).collect();
    let batch = &converted[..];
    let prepared = &prepared[..];
    // Chains: each block continues the previous one when it starts from its end state.
    // Blocks carry their index in the batch.
    let mut chains: Vec<Vec<(usize, &BatchBlock)>> = Vec::new();
    // The first block known to be invalid: nothing after it is worth replaying.
    let first_bad = std::sync::atomic::AtomicUsize::new(usize::MAX);
    let ceiling = rules().max_frames_any();
    for (i, b) in batch.iter().enumerate() {
        // A run longer than any difficulty allows at its gap to the parent (difficulty 1 allows the
        // most) can't be valid: not worth a replay, whatever it claims (the sequential check rejects it).
        let bound = match b.parent {
            Parent::Block { gap_seconds, .. } if gap_seconds != u64::MAX => ceiling.min(rules().max_frames(1, gap_seconds)),
            _ => ceiling,
        };
        let start = match proof::split_blob(&b.blob) {
            Ok((frames, start, ..)) if frames <= bound && frames > 0 => start,
            _ => {
                first_bad.fetch_min(i, std::sync::atomic::Ordering::Relaxed);
                break;
            }
        };
        let parent_id = match b.parent {
            Parent::Block { id, .. } => Some(id),
            Parent::Genesis => None,
        };
        let continues = start == Start::Parent && chains.last().and_then(|c| c.last()).is_some_and(|(_, l)| Some(l.id) == parent_id);
        if continues {
            chains.last_mut().unwrap().push((i, b));
        } else if start == Start::PowerOn || parent_id.is_none() || parent_id.is_some_and(|p| cached(&p).is_some()) {
            chains.push(vec![(i, b)]);
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
            let _ = replay_chain(chain, prepared, state_ok, &replayed, &first_bad);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::scope(|sc| {
        for _ in 0..threads.max(1).min(chains.len()) {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(chain) = chains.get(i) else { break };
                let _ = replay_chain(chain, prepared, state_ok, &replayed, &first_bad);
            });
        }
    });
    replayed.into_inner()
}

fn replay_chain(chain: &[(usize, &BatchBlock)], prepared: &[crate::chain::Prepared], state_ok: bool, replayed: &std::sync::atomic::AtomicUsize, first_bad: &std::sync::atomic::AtomicUsize) -> Result<()> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut prev: Option<(SaveState, Progress, bool)> = None;
    for (k, (i, b)) in chain.iter().enumerate() {
        if *i > first_bad.load(Relaxed) {
            return Ok(()); // an earlier block of the batch is invalid
        }
        let start = proof::proof_start(&b.blob)?;
        let key = run_key(&b.parent, start, &b.seed, &prepared[*i].key_src);
        let last = k + 1 == chain.len();
        if known_invalid(&key).is_some() {
            first_bad.fetch_min(*i, Relaxed);
            return Ok(());
        }
        if known_summary(&key).is_some() {
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
                // Its state is gone: replay it below like any other.
                None => {}
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
        replayed.fetch_add(1, Relaxed);
        REPLAYS.fetch_add(1, Relaxed);
        let (g, now, frames) = match replay_blob_committed(game, before, &b.seed, &b.blob, &prepared[*i].hdr, state_ok)? {
            Ok(x) => x,
            Err(None) => {
                // A bad encoding: not this block's verdict, and not worth remembering; stop here.
                first_bad.fetch_min(*i, Relaxed);
                return Ok(());
            }
            Err(Some(why)) => {
                // The sequential check rejects it (and the rest of the batch).
                remember_invalid(key, &why);
                first_bad.fetch_min(*i, Relaxed);
                return Ok(());
            }
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
