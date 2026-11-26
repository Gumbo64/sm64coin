//! Stage C and the remaining v0.4 wiring, the node's side (layer 5; docs/V0.4-DESIGN.md sections
//! 2.5-2.6, 3.3, 6): counting a block's weighted proofs without a replay, the sync pre-pass for
//! records, a small cache of recently decoded inputs so state-coded records can be re-encoded for
//! peers, and when the template for a tip should be built. Everything here is node policy or a
//! structural re-derivation of what [`crate::included`] already judged; nothing decides validity.
//!
//! The pure consensus pieces (proof outputs, the coinbase rule, the weighted fork choice) are in
//! records.rs and equivocation.rs.
//!
//! Read next: ffi.rs.

use crate::admit::Bounded;
use crate::included::Parent1;
use crate::records::{self, Record};
use std::sync::Mutex;

// ---- weight without a replay ----

/// How many of the block's records count for weight (the weight window), from the structure
/// alone: the leaves give the committed root, the rules of `records::check` except the start (the
/// parent's end state may not be at hand after a restart). For blocks already accepted. 0 when
/// `proof_weight` is off, there are no records, or anything fails.
pub fn count_records(field: &[u8], nonce: &[u8], h: &Parent1) -> u32 {
    let feats = crate::features::active();
    if !feats.proof_weight || !feats.included_proofs || field.is_empty() {
        return 0;
    }
    let Some(fields) = records::nonce_fields(nonce) else { return 0 };
    let Ok(recs) = records::decode(field) else { return 0 };
    let Ok(winner) = h.winner() else { return 0 };
    match records::check_opts(&recs, fields.root.as_ref(), &winner, None, &records::PROPOSED) {
        Ok((_, counted)) => counted as u32,
        Err(_) => 0,
    }
}

// ---- recently decoded inputs, for re-encoding state-coded records for peers ----

const INPUTS_KEPT: usize = 256;

fn inputs() -> &'static Mutex<Bounded<[u8; 32], std::sync::Arc<Vec<u8>>>> {
    static I: std::sync::OnceLock<Mutex<Bounded<[u8; 32], std::sync::Arc<Vec<u8>>>>> = std::sync::OnceLock::new();
    I.get_or_init(|| Mutex::new(Bounded::new(INPUTS_KEPT)))
}

/// Remembers a run's inputs (4 bytes per non-poked frame) by its input hash.
pub fn note_inputs(input_hash: [u8; 32], pads: &[sm64_binds::GamePad]) {
    let raw: Vec<u8> = pads.iter().flat_map(crate::proof::pad_bytes).collect();
    inputs().lock().unwrap_or_else(|e| e.into_inner()).insert(input_hash, std::sync::Arc::new(raw));
}

pub fn inputs_for(input_hash: &[u8; 32]) -> Option<std::sync::Arc<Vec<u8>>> {
    inputs().lock().unwrap_or_else(|e| e.into_inner()).get(input_hash).cloned()
}

/// The records `field` with every record proof in an encoding a peer with codec set `mask`
/// decodes. Ok(None): no change needed. Err: the field doesn't parse, or a state-coded record's
/// inputs are not cached (the caller sends the block as it is).
pub fn field_for_peer(field: &[u8], mask: &crate::codecs::Mask) -> Result<Option<Vec<u8>>, String> {
    if field.is_empty() {
        return Ok(None);
    }
    let mut recs = records::decode(field).map_err(|e| format!("the proofs field does not parse: {e}"))?;
    let mut changed = false;
    for (i, r) in recs.iter_mut().enumerate() {
        if crate::codecs::need(&r.proof, mask) == 0 {
            continue;
        }
        let h = crate::commit::header(&r.proof).map_err(|e| format!("record {i}: {e}"))?;
        let raw = inputs_for(&h.input_hash);
        match crate::codecs::transcode(&r.proof, raw.as_deref().map(|v| v.as_slice()), mask) {
            Ok(Some(b)) => {
                r.proof = b;
                changed = true;
            }
            Ok(None) => {}
            Err(e) => return Err(format!("record {i}: {e:#}")),
        }
    }
    Ok(if changed { Some(records::encode(&recs)) } else { None })
}

// ---- when to build the template ----

/// Unix second at which the template on `tip` should be built: the tip's eligible time (pof.rs)
/// plus the displacement window and the collection grace. 0: the tip is unknown to pof.rs.
pub fn records_ready_at(tip: &[u8; 32]) -> u64 {
    crate::pof::policy().lock().unwrap_or_else(|e| e.into_inner()).seen(tip).map_or(0, |s| s.at + crate::pof::POF_WINDOW_SECONDS + records::COLLECTION_GRACE_SECONDS)
}

// ---- the sync pre-pass for records ----

/// A block of a sync batch for [`preverify_records`].
pub struct SyncBlock<'a> {
    pub id: [u8; 32],
    pub blob: &'a [u8],
    /// The block's own retarget difficulty (0: unknown, the window is not applied).
    pub difficulty: u64,
}

struct Parsed {
    id: [u8; 32],
    prev_id: [u8; 32],
    seed: [u8; 32],
    proof: Vec<u8>,
    records: Vec<Record>,
    difficulty: u64,
}

#[cfg(not(target_arch = "wasm32"))]
mod prepass {
    use super::*;
    use crate::cache::{Summary, cached, info, known_invalid, known_summary, remember_invalid, remember_summary, run_key};
    use crate::emulator::generator;
    use crate::game::read_progress;
    use crate::proof::Start;
    use crate::replay::{EndState, Parent, replay_blob_committed};
    use crate::seed::Seed;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    use std::sync::Arc;

    /// The state records start from: power-on, or `state` (the end state of B_{h-1}).
    enum Origin {
        PowerOn,
        State(Arc<EndState>),
    }

    pub fn parse(blocks: &[SyncBlock]) -> Vec<Option<Parsed>> {
        blocks
            .iter()
            .map(|b| {
                let s = crate::template::split_sealed(b.blob).ok()?;
                let f = crate::template::facts(&s.parts).ok()?;
                let records = if s.field.is_empty() { vec![] } else { records::decode(&s.field).ok()? };
                Some(Parsed { id: b.id, prev_id: f.prev_id, seed: f.seed, proof: s.proof, records, difficulty: b.difficulty })
            })
            .collect()
    }

    /// Replays one record from `from`; true if it ran a replay.
    fn replay_record(prev_id: [u8; 32], from: &Origin, rec: &Record, state_ok: bool, ceiling: u32) -> bool {
        let Ok(f) = rec.facts() else { return false };
        let Ok(p) = crate::chain::prepare(&rec.proof, ceiling, state_ok) else { return false };
        let parent = Parent::Block { id: prev_id, gap_seconds: 0 };
        let start = p.hdr.start;
        let seed = Seed(f.seed);
        let key = run_key(&parent, start, &seed, &p.key_src);
        if known_summary(&key).is_some() || known_invalid(&key).is_some() {
            return false;
        }
        let (game, before) = match (start, from) {
            (Start::PowerOn, Origin::PowerOn) => {
                let Ok(g) = generator().and_then(|g| g.create_game()) else { return false };
                let Ok(p) = read_progress(&g) else { return false };
                (g, p)
            }
            (Start::Parent, Origin::State(st)) => {
                let Ok(mut g) = generator().and_then(|g| g.create_game()) else { return false };
                if g.load_state(&st.game).is_err() {
                    return false;
                }
                (g, st.progress)
            }
            _ => return false, // not the natural start: the sequential check refuses it
        };
        crate::cache::REPLAYS.fetch_add(1, Relaxed);
        let Ok(res) = replay_blob_committed(game, before, &seed, &p.layout, &p.hdr, state_ok) else { return true };
        let id = f.leaf(&[0; 64]);
        match res {
            Ok((g, now, frames)) => {
                let ends_cycle = crate::rules::active().ends_cycle(&now);
                remember_summary(id, key, Summary { frames, start, progress: now, ends_cycle, event: crate::game::event(&before, &now) });
                drop(g);
            }
            Err(Some(why)) => remember_invalid(key, &why),
            Err(None) => {}
        }
        true
    }

    /// Replays `recs` (all from the same state) on up to `threads` threads.
    fn replay_records(prev_id: [u8; 32], from: &Origin, recs: &[Record], threads: usize, state_ok: bool, ceiling: u32, bound: u32, replayed: &AtomicUsize) {
        let next = AtomicUsize::new(0);
        let work = || loop {
            let i = next.fetch_add(1, Relaxed);
            let Some(r) = recs.get(i) else { break };
            if r.facts().map_or(true, |f| f.run.frames == 0 || f.run.frames > bound) {
                continue;
            }
            if replay_record(prev_id, from, r, state_ok, ceiling) {
                replayed.fetch_add(1, Relaxed);
            }
        };
        std::thread::scope(|sc| {
            for _ in 1..threads.clamp(1, recs.len().max(1)) {
                sc.spawn(work);
            }
            work();
        });
    }

    /// Walks one segment (consecutive blocks, each the parent of the next) and replays the
    /// records of every block whose parent h and grandparent are in or known to the walk.
    fn walk(seg: &[usize], ps: &[Parsed], threads: usize, state_ok: bool, replayed: &AtomicUsize) {
        let ceiling = crate::rules::active().max_frames_any();
        let first = &ps[seg[0]];
        // The end state of the block before the segment: cached or unknown.
        let outside = cached(&first.prev_id);
        // (state after the previous block, its id, ends_cycle) as the walk goes.
        let mut before: Option<(Arc<EndState>, [u8; 32])> = outside.map(|s| (s, first.prev_id));
        for (k, &i) in seg.iter().enumerate() {
            let b = &ps[i];
            // Records of the NEXT block (it includes proofs for block b's height; they start from
            // the end state of the block before b).
            if let Some(&nx) = seg.get(k + 1) {
                let nb = &ps[nx];
                if !nb.records.is_empty() {
                    let from = match &before {
                        Some((s, _)) if !s.ends_cycle => Some(Origin::State(s.clone())),
                        Some(_) => Some(Origin::PowerOn),
                        None => info(&b.prev_id).filter(|x| x.1).map(|_| Origin::PowerOn),
                    };
                    if let Some(from) = from {
                        let winner_frames = crate::commit::header(&b.proof).map_or(0, |h| h.frames);
                        let bound = if b.difficulty == 0 {
                            ceiling
                        } else {
                            records::window_frames(b.difficulty, winner_frames, records::PROPOSED.archive_seconds).min(ceiling)
                        };
                        replay_records(b.prev_id, &from, &nb.records, threads, state_ok, ceiling, bound, replayed);
                    }
                }
            }
            // The end state of b, for the next steps (only while records of later blocks need it).
            if !seg.get(k + 2..).is_some_and(|r| r.iter().any(|&j| !ps[j].records.is_empty())) {
                return;
            }
            let Ok(pre) = crate::chain::prepare(&b.proof, ceiling, state_ok) else { return };
            let start = pre.hdr.start;
                        let g = match (start, &before) {
                (Start::PowerOn, _) => generator().and_then(|g| g.create_game()),
                (Start::Parent, Some((s, _))) if !s.ends_cycle => generator().and_then(|g| g.create_game()).and_then(|mut g| g.load_state(&s.game).map(|_| g)),
                _ => return,
            };
            let Ok(g) = g else { return };
            let Ok(p0) = (match (start, &before) {
                (Start::Parent, Some((s, _))) => Ok(s.progress),
                _ => read_progress(&g),
            }) else {
                return;
            };
            crate::cache::REPLAYS.fetch_add(1, Relaxed);
            let Ok(Ok((g, now, _frames))) = replay_blob_committed(g, p0, &Seed(b.seed), &pre.layout, &pre.hdr, state_ok) else { return };
            let ends_cycle = crate::rules::active().ends_cycle(&now);
            let st = EndState { game: g.game.save_state(), progress: now, ends_cycle, event: crate::game::event(&p0, &now) };
            before = Some((Arc::new(st), b.id));
        }
    }

    pub fn run(blocks: &[SyncBlock], threads: usize) -> usize {
        let feats = crate::features::active();
        if !feats.included_proofs || blocks.is_empty() {
            return 0;
        }
        let parsed = parse(blocks);
        // Only blocks that parse can be walked; a segment ends at a block that doesn't.
        let mut ps: Vec<Parsed> = Vec::new();
        let mut segs: Vec<Vec<usize>> = Vec::new();
        for p in parsed {
            let Some(p) = p else {
                segs.push(vec![]);
                continue;
            };
            let continues = ps.last().is_some_and(|l: &Parsed| l.id == p.prev_id) && segs.last().is_some_and(|s| !s.is_empty());
            ps.push(p);
            if continues {
                segs.last_mut().unwrap().push(ps.len() - 1);
            } else {
                segs.push(vec![ps.len() - 1]);
            }
        }
        // Only segments where some block after the first has records.
        segs.retain(|s| s.iter().skip(1).any(|&i| !ps[i].records.is_empty()));
        if segs.is_empty() {
            return 0;
        }
        let state_ok = crate::statecodec::state_codec_enabled();
        let replayed = AtomicUsize::new(0);
        let next = AtomicUsize::new(0);
        let workers = threads.max(1).min(segs.len());
        let inner = threads.max(1).div_ceil(workers);
        std::thread::scope(|sc| {
            for _ in 0..workers {
                sc.spawn(|| loop {
                    let i = next.fetch_add(1, Relaxed);
                    let Some(seg) = segs.get(i) else { break };
                    walk(seg, &ps, inner, state_ok, &replayed);
                });
            }
        });
        replayed.into_inner()
    }
}

/// The records pre-pass of a sync batch in chain order: for every block whose records are
/// non-empty and whose parent block h is the previous block of the batch, replays the records in
/// parallel from B_{h-1}'s end state (cached, or walked to from the segment's start), keeping
/// their run summaries so the sequential check (`chain::verify_records`) finds each run. Records
/// outside the archive window of block h (when its difficulty is known) or past the ceiling are
/// not replayed. Returns how many records it replayed. Never decides validity. 0 with included
/// proofs off.
///
/// Cost note: it re-walks the batch's blocks from each segment's start to get the intermediate
/// end states (the block pre-pass keeps only each chain's last), so a batch with proofs costs its
/// blocks twice plus the records once, in parallel.
pub fn preverify_records(blocks: &[SyncBlock], threads: usize) -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        prepass::run(blocks, threads)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (blocks, threads);
        0
    }
}
