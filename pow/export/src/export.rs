//! The export: replay the chain, write effective inputs, cycles, segments.
//!
//! For each block, in height order:
//! 1. **Chain replay** (`sm64pow::chain::played_wanted_block`, the consensus
//!    code): the pad played on every frame (pokes applied) and which frames
//!    were poked, plus the block's end state.
//! 2. **Plain replay**: the block's start state (parent's end state, or
//!    power-on) loaded into a fresh game, the effective pads stepped with
//!    `SM64Game::step_game` (NO perturbation), per-frame features read. Its
//!    final state must be byte-identical to the chain replay's end state: that
//!    is the "pokes only touch the pad" check, on the whole 16 MB RAM image,
//!    at every block.
//! 3. Files: see `docs/EXPORT.md`.

use crate::features;
use crate::m64::{self, Header, Rom};
use crate::source::BlockRec;
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Serialize;
use sm64_binds::{GamePad, SM64Game, SaveState};
use sm64pow::chain::{self, BlockVerdict, Parent};
use sm64pow::game::{Progress, read_progress};
use sm64pow::proof::{self, Start};
use sm64pow::seed::Seed;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum States {
    None,
    Portable,
    Raw,
    Both,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub out: PathBuf,
    pub rom: Rom,
    pub features: bool,
    pub states: States,
    /// Also write `cycle_N.mupen64plus.m64` with one leading zero poll (Mupen64Plus polls once more at boot).
    pub mupen64plus: bool,
    /// Clear the reserved button bits (both set: Mupen64Plus resets the console) in the `.m64` movies;
    /// the raw `.pads` files stay as played.
    pub clear_reserved_bits: bool,
    /// Which consensus rules to replay with (`V04`: committed-input proofs and included proofs).
    pub format: Format,
    /// v0.4: also write each included proof's m64 (the cycle's frames up to the block, then the proof's
    /// frames: a movie from power-on that Mupen64Plus can play). Large: the prefix repeats per proof.
    pub record_m64: bool,
    pub author: String,
}

/// The chain's rules: `Legacy` = v0.2 / v0.3 blocks (proof layout of the old chains, no included
/// proofs), `V04` = v0.4 and later (committed-input headers, included proofs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Legacy,
    V04,
}

impl Format {
    /// Sets the process-wide consensus switches (`sm64pow::features`) for this format.
    pub fn apply(self) {
        let f = match self {
            Format::Legacy => sm64pow::features::Features::NONE,
            Format::V04 => sm64pow::features::Features::ALL,
        };
        sm64pow::features::set(f).expect("valid feature set");
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Prog {
    pub stars: u32,
    pub keys: u32,
    pub grand_star: bool,
    /// The cap switches pressed (bit 0 wing, 1 metal, 2 vanish).
    pub caps: u8,
    pub points: u32,
}

impl From<Progress> for Prog {
    fn from(p: Progress) -> Prog {
        Prog { stars: p.stars, keys: p.keys, grand_star: p.grand_star, caps: p.caps, points: p.points() }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct BlockMeta {
    pub height: u64,
    pub id: String,
    pub prev: String,
    pub timestamp: u64,
    pub seed: String,
    pub difficulty: Option<u64>,
    pub miner_key: Option<String>,
    pub sig: Option<String>,
    /// `genesis` (first block, from power-on), `cycle_start` (power-on after a Grand Star),
    /// `restart` (power-on mid-cycle: the cycle so far is abandoned), `power_on` (power-on,
    /// parent not in the dump), `parent` (from the parent's end state).
    pub start_kind: String,
    pub proof_codec: u8,
    pub proof_bytes: usize,
    pub frames: u32,
    pub poked_frames: u32,
    /// Bit k (LSB first within each byte) set: frame k's pad was a poke. Hex.
    pub poked_mask_hex: String,
    pub progress_before: Prog,
    pub progress_after: Prog,
    /// "c09s1" (course 9 star 1), "c00s3" (castle), "key1", "grand".
    pub event: Option<String>,
    pub course: Option<u8>,
    pub star: Option<u8>,
    /// What the game says at the end of the block.
    pub end_course_num: i32,
    pub end_level_num: i32,
    pub end_area_index: i32,
    pub end_act_num: i32,
    pub end_pos: [f32; 3],
    pub start_state_hash: String,
    pub end_state_hash: String,
    pub cycle: u32,
    /// Frames of the cycle before this block (its offset in the cycle's m64).
    pub cycle_frame_offset: u32,
    /// v0.4: the payout address this block's template committed (`flag || spend || view`, hex).
    pub payout: Option<String>,
    /// v0.4: how many proof records this block carries (they are for the PARENT's height; they are
    /// exported with the parent's block).
    pub proofs_carried: u32,
    /// v0.4: how many other runs of THIS height the next block archives (exported as segments under
    /// `records/`); None when the next block is not in the input.
    pub proofs_archived: Option<u32>,
    pub files: BlockFiles,
}

/// One archived run of a height (a v0.4 included proof): a segment from the same start as the
/// height's block, exported like a block.
#[derive(Serialize, Clone, Debug)]
pub struct RecordMeta {
    /// The height the run is a proof for (the including block is `included_in_height` = height + 1).
    pub height: u64,
    pub index: u32,
    pub included_in_height: u64,
    pub included_in_id: String,
    pub input_hash: String,
    pub seed: String,
    pub miner_key: String,
    /// The payout address the run's template commits (`flag || spend || view`, hex).
    pub payout: String,
    pub sig: String,
    pub proof_codec: Option<u8>,
    pub proof_bytes: usize,
    pub frames: u32,
    pub poked_frames: u32,
    pub poked_mask_hex: String,
    /// `power_on` or `parent` (the height's natural start).
    pub start_kind: String,
    pub start_state_hash: String,
    /// Whether the start state is the same as the block of this height's (`blocks/` start files apply).
    pub start_same_as_block: bool,
    /// Whether the run counted for fork choice weight in its including block (None: not computable,
    /// e.g. the dump has no difficulty). The weight window is `records::PROPOSED.weight_seconds` of
    /// auction time after the winner's due time.
    pub counted_for_weight: Option<bool>,
    /// Frames longer (+) or shorter (-) than the height's block.
    pub frames_vs_winner: i64,
    pub progress_after: Prog,
    pub event: Option<String>,
    pub course: Option<u8>,
    pub star: Option<u8>,
    pub end_course_num: i32,
    pub end_level_num: i32,
    pub end_area_index: i32,
    pub end_act_num: i32,
    pub end_pos: [f32; 3],
    pub end_state_hash: String,
    pub cycle: u32,
    /// Where the proof's frames start in `files.m64` (the cycle's frames before the block), when written.
    pub m64_frame_offset: Option<u32>,
    pub files: RecordFiles,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct RecordFiles {
    pub meta: String,
    pub pads: String,
    pub features: Option<String>,
    pub m64: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct BlockFiles {
    pub pads: String,
    pub features: Option<String>,
    pub start_portable: Option<String>,
    /// Whether a game loaded from `start_portable` and fed this block's pads ends in the chain's
    /// end state (same game state, same variables). False: use `start_raw` for this block.
    pub start_portable_exact: Option<bool>,
    pub start_raw: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct CycleMeta {
    pub cycle: u32,
    pub m64: String,
    pub mupen64plus_m64: Option<String>,
    pub frames: u32,
    pub first_height: u64,
    pub last_height: u64,
    /// `grand_star` (the cycle was completed), `restarted` (a later block restarted from power-on), `open` (the dump ended).
    pub end: String,
    pub blocks: Vec<CycleBlock>,
}

#[derive(Serialize, Clone, Debug)]
pub struct CycleBlock {
    pub height: u64,
    pub id: String,
    pub frames: u32,
    /// Frame count of the cycle after this block: where the boundary is.
    pub end_frame: u32,
    /// `cn_fast_hash` of the chain replay's end state (the whole RAM image): what `verify` compares.
    pub end_state_hash: String,
}

/// The dataset layout's version (manifest `schema_version`): 2 adds v0.4 records, `rules`, snapshot manifests.
pub const SCHEMA_VERSION: u32 = 2;

pub fn hash(s: &SaveState) -> String {
    hex::encode(sm64pow::sign::cn_fast_hash(s.as_bytes()))
}

fn pack_mask(poked: &[bool]) -> String {
    let mut b = vec![0u8; poked.len().div_ceil(8)];
    for (i, &p) in poked.iter().enumerate() {
        if p {
            b[i / 8] |= 1 << (i % 8);
        }
    }
    hex::encode(b)
}

pub fn unpack_mask(h: &str, frames: usize) -> Result<Vec<bool>> {
    let b = hex::decode(h)?;
    ensure!(b.len() == frames.div_ceil(8), "mask length");
    Ok((0..frames).map(|i| b[i / 8] >> (i % 8) & 1 == 1).collect())
}

fn gz(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(data)?;
    Ok(e.finish()?)
}

fn write(p: &Path, data: &[u8]) -> Result<()> {
    std::fs::write(p, data).with_context(|| format!("writing {}", p.display()))
}

pub fn fresh_game() -> Result<SM64Game> {
    sm64pow::generator()?.create_game()
}

/// Plays `pads` on `game` WITHOUT any perturbation, optionally recording features.
pub fn play_plain(game: &mut SM64Game, pads: &[GamePad], mut feats: Option<&mut Vec<u8>>) -> Result<()> {
    if let Some(f) = feats.as_deref_mut() {
        f.extend_from_slice(&features::record(game)?);
    }
    for p in pads {
        game.step_game(*p)?;
        if let Some(f) = feats.as_deref_mut() {
            f.extend_from_slice(&features::record(game)?);
        }
    }
    Ok(())
}

struct Cycle {
    n: u32,
    pads: Vec<GamePad>,
    blocks: Vec<CycleBlock>,
    first_ts: u64,
}

pub struct Summary {
    pub blocks: usize,
    pub cycles: usize,
    pub frames: u64,
    pub poked: u64,
    /// v0.4 included proofs exported as segments, their frames, and how many counted for weight.
    pub records: usize,
    pub record_frames: u64,
    pub records_counted: usize,
    pub plain_replay_mismatches: Vec<u64>,
    pub portable_mismatches: Vec<u64>,
}

pub fn export(blocks: &[BlockRec], o: &Options, log: &mut dyn FnMut(&str)) -> Result<Summary> {
    ensure!(!blocks.is_empty(), "no blocks");
    for d in ["blocks", "cycles", "records"] {
        std::fs::create_dir_all(o.out.join(d))?;
    }
    write(&o.out.join("features.schema.json"), serde_json::to_string_pretty(&features::schema())?.as_bytes())?;

    o.format.apply();
    let mut sum = Summary { blocks: 0, cycles: 0, frames: 0, poked: 0, records: 0, record_frames: 0, records_counted: 0, plain_replay_mismatches: vec![], portable_mismatches: vec![] };
    let mut records_meta: Vec<RecordMeta> = Vec::new();
    let mut cycles_meta: Vec<CycleMeta> = Vec::new();
    let mut cur: Option<Cycle> = None;
    // The previous block: its end state, id, timestamp, progress, whether it ended the cycle.
    let mut prev: Option<(u64, [u8; 32], u64, SaveState, Progress, bool)> = None;
    let mut n_cycles = 0u32;

    for (bi, b) in blocks.iter().enumerate() {
        if let Some((h, id, ..)) = &prev {
            ensure!(b.height == h + 1 && b.prev == *id, "block {} does not extend block {h}: the input must be one chain in height order", b.height);
        }
        let start = proof::proof_start(&b.proof).map_err(|e| anyhow!("block {}: {e}", b.height))?;
        let (_, _, codec, _) = proof::split_blob(&b.proof)?;
        let parent = if b.height == 1 { Parent::Genesis } else { Parent::Block { id: b.prev, gap_seconds: prev.as_ref().map(|p| b.ts.saturating_sub(p.2)).unwrap_or(0) } };
        let played = match played_block(o.format, &b.id, parent, &Seed(b.seed), &b.proof).with_context(|| format!("replaying block {}", b.height))? {
            Ok(p) => p,
            Err(BlockVerdict::NeedParent) => bail!("block {} continues from its parent's end state, which is not in the input: start the export at a block that starts from power-on (or at height 1)", b.height),
            Err(v) => bail!("block {} does not replay: {v:?}", b.height),
        };
        let end = chain::cached(&b.id).ok_or_else(|| anyhow!("block {}: end state not cached", b.height))?;
        let frames = played.pads.len() as u32;

        // Start state and kind.
        let (kind, start_state, before): (&str, SaveState, Progress) = match (start, &prev) {
            (Start::PowerOn, None) => (if b.height == 1 { "genesis" } else { "power_on" }, fresh_game()?.save_state(), Progress::default()),
            (Start::PowerOn, Some((.., ends))) => (if *ends { "cycle_start" } else { "restart" }, fresh_game()?.save_state(), Progress::default()),
            (Start::Parent, Some((_, _, _, s, p, _))) => ("parent", s.clone(), *p),
            (Start::Parent, None) => bail!("block {} starts from a parent that is not in the input", b.height),
        };
        let before = if start == Start::PowerOn { read_progress(&fresh_game()?)? } else { before };

        // New cycle?
        if start == Start::PowerOn {
            if let Some(c) = cur.take() {
                let end = if prev.as_ref().map(|p| p.5).unwrap_or(false) { "grand_star" } else { "restarted" };
                cycles_meta.push(write_cycle(o, c, end)?);
            }
            cur = Some(Cycle { n: n_cycles, pads: vec![], blocks: vec![], first_ts: b.ts });
            n_cycles += 1;
        }
        let cyc = cur.as_mut().ok_or_else(|| anyhow!("block {} has no cycle", b.height))?;

        // Plain replay: effective pads, no perturbation.
        let mut game = fresh_game()?;
        game.load_state(&start_state)?;
        let mut feats = Vec::new();
        play_plain(&mut game, &played.pads, if o.features { Some(&mut feats) } else { None })?;
        let plain_end = game.save_state();
        if plain_end.as_bytes() != end.game.as_bytes() {
            log(&format!("MISMATCH block {}: the plain replay of the effective pads ends in a different RAM image than the chain replay", b.height));
            sum.plain_replay_mismatches.push(b.height);
        }
        let gs = game.get_game_state()?;

        // Files.
        let name = format!("h{:07}_{}", b.height, hex::encode(&b.id[..4]));
        let mut files = BlockFiles { pads: format!("blocks/{name}.pads"), ..Default::default() };
        write(&o.out.join(&files.pads), &m64::frames_bytes(&played.pads))?;
        if o.features {
            files.features = Some(format!("blocks/{name}.features.bin"));
            write(&o.out.join(files.features.as_ref().unwrap()), &feats)?;
        }
        if matches!(o.states, States::Portable | States::Both) {
            let mut g = fresh_game()?;
            g.load_state(&start_state)?;
            let p = g.export_portable().map_err(|e| anyhow!("{e}"))?;
            let f = format!("blocks/{name}.start.portable.gz");
            write(&o.out.join(&f), &gz(&p.to_bytes())?)?;
            files.start_portable = Some(f);
            // Round trip: load the portable state into a fresh game and play the same pads. The end
            // must agree with the chain's end state: the same game state (`GameState`, progress)
            // and the same variables (`Portable::digest`, which ignores heap addresses: a loaded
            // game's heap is laid out differently from the one it was exported from).
            let mut h = fresh_game()?;
            h.load_portable(&p).map_err(|e| anyhow!("{e}"))?;
            play_plain(&mut h, &played.pads, None)?;
            let mut e = fresh_game()?;
            e.load_state(&end.game)?;
            let (dh, de) = (h.export_portable().map_err(|e| anyhow!("{e}"))?.digest(), e.export_portable().map_err(|e| anyhow!("{e}"))?.digest());
            // gMatStack is the render walk's scratch matrices (recomputed every frame, never read by
            // the simulation); a loaded game's first frames leave different leftovers there.
            let names: Vec<_> = (0..dh.len().min(de.len()))
                .filter(|&i| dh[i] != de[i])
                .map(sm64_binds::portable::Portable::var_name)
                .filter(|n| !n.contains("gMatStack"))
                .collect();
            let same_vars = names.is_empty() && dh.len() == de.len();
            if !same_vars {
                log(&format!("block {}: portable variables that differ after the replay: {:?}", b.height, &names[..names.len().min(8)]));
            }
            let same_gs = h.get_game_state()? == e.get_game_state()? && read_progress(&h)? == end.progress;
            if !same_vars || !same_gs {
                log(&format!("MISMATCH block {}: replay from the portable start state ends differently (variables equal: {same_vars}, game state equal: {same_gs})", b.height));
                sum.portable_mismatches.push(b.height);
            }
            files.start_portable_exact = Some(same_vars && same_gs);
        }
        if matches!(o.states, States::Raw | States::Both) {
            let f = format!("blocks/{name}.start.state.gz");
            write(&o.out.join(&f), &gz(start_state.as_bytes())?)?;
            files.start_raw = Some(f);
        }

        // v0.4 included proofs of this height (carried by the next block): same start as this block.
        let mut proofs_archived = None;
        if o.format == Format::V04 {
            if let Some(next) = blocks.get(bi + 1).filter(|n| n.height == b.height + 1) {
                let w = RecordCtx {
                    block: b,
                    next,
                    parent,
                    kind,
                    block_start_hash: hash(&start_state),
                    block_frames: frames,
                    block_proof: &b.proof,
                    parent_end: prev.as_ref().map(|p| &p.3),
                    cycle: cyc.n,
                    cycle_prefix: &cyc.pads,
                    first_ts: cyc.first_ts,
                };
                let ms = export_records(o, &w, log, &mut sum)?;
                proofs_archived = Some(ms.len() as u32);
                records_meta.extend(ms);
            }
        }
        let (ev, course, star) = match end.event {
            Some(e) => {
                let (c, s) = match e {
                    sm64pow::game::Event::Star { course, star } => (Some(course), Some(star)),
                    _ => (None, None),
                };
                (Some(e.name()), c, s)
            }
            None => (None, None, None),
        };
        let poked = played.poked.iter().filter(|p| **p).count() as u32;
        let end_hash = hash(&end.game);
        let meta = BlockMeta {
            height: b.height,
            id: hex::encode(b.id),
            prev: hex::encode(b.prev),
            timestamp: b.ts,
            seed: hex::encode(b.seed),
            difficulty: b.difficulty,
            miner_key: b.miner_key.map(hex::encode),
            sig: b.sig.map(hex::encode),
            start_kind: kind.into(),
            proof_codec: codec,
            proof_bytes: b.proof.len(),
            frames,
            poked_frames: poked,
            poked_mask_hex: pack_mask(&played.poked),
            progress_before: before.into(),
            progress_after: end.progress.into(),
            event: ev,
            course,
            star,
            end_course_num: gs.course_num,
            end_level_num: gs.level_num,
            end_area_index: gs.area_index,
            end_act_num: gs.act_num,
            end_pos: gs.pos,
            start_state_hash: hash(&start_state),
            end_state_hash: end_hash.clone(),
            cycle: cyc.n,
            cycle_frame_offset: cyc.pads.len() as u32,
            payout: b.payout.as_ref().map(|p| hex::encode(crate::source::payout_bytes(p))),
            proofs_carried: if b.records.is_empty() { 0 } else { sm64pow::records::decode(&b.records).map(|r| r.len() as u32).unwrap_or(0) },
            proofs_archived,
            files,
        };
        write(&o.out.join(format!("blocks/{name}.json")), serde_json::to_string_pretty(&meta)?.as_bytes())?;

        cyc.pads.extend_from_slice(&played.pads);
        cyc.blocks.push(CycleBlock { height: b.height, id: hex::encode(b.id), frames, end_frame: cyc.pads.len() as u32, end_state_hash: end_hash });
        sum.blocks += 1;
        sum.frames += frames as u64;
        sum.poked += poked as u64;
        log(&format!("block {} cycle {} {} {} frames ({} poked) {}", b.height, cyc.n, kind, frames, poked, meta.event.as_deref().unwrap_or("")));
        prev = Some((b.height, b.id, b.ts, end.game.clone(), end.progress, end.ends_cycle));
    }
    if let Some(c) = cur.take() {
        let end = if prev.as_ref().map(|p| p.5).unwrap_or(false) { "grand_star" } else { "open" };
        cycles_meta.push(write_cycle(o, c, end)?);
    }
    sum.cycles = cycles_meta.len();
    let manifest = serde_json::json!({
        "format": "sm64coin-export-v1",
        "schema_version": SCHEMA_VERSION,
        "rules": match o.format { Format::Legacy => "legacy", Format::V04 => "v0.4" },
        "game_build": sm64pow::GAME_BUILD,
        "rom": {"name": String::from_utf8_lossy(&o.rom.name), "crc1": format!("{:08x}", o.rom.crc1)},
        "blocks": sum.blocks, "frames": sum.frames, "poked_frames": sum.poked,
        "records": sum.records, "record_frames": sum.record_frames, "records_counted_for_weight": sum.records_counted,
        "record_m64": o.record_m64,
        "record_files": records_meta.iter().map(|r| r.files.meta.clone()).collect::<Vec<_>>(),
        "first_height": blocks[0].height, "last_height": blocks.last().unwrap().height,
        "features": o.features, "mupen64plus_lead_poll": o.mupen64plus,
        "cycles": cycles_meta,
    });
    write(&o.out.join("manifest.json"), serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    Ok(sum)
}

fn write_cycle(o: &Options, c: Cycle, end: &str) -> Result<CycleMeta> {
    let desc = format!("SM64Coin cycle {} (blocks {}-{}): effective inputs, pokes applied", c.n, c.blocks[0].height, c.blocks.last().unwrap().height);
    let h = Header::power_on(0, c.first_ts as u32, o.rom.clone(), &o.author, &desc);
    let f = format!("cycles/cycle_{:04}.m64", c.n);
    let movie = if o.clear_reserved_bits { m64::clear_reserved_bits(&c.pads) } else { c.pads.clone() };
    write(&o.out.join(&f), &m64::write(&h, &movie))?;
    let mp = if o.mupen64plus {
        let f = format!("cycles/cycle_{:04}.mupen64plus.m64", c.n);
        let mut pads = vec![GamePad::default()];
        pads.extend_from_slice(&movie);
        write(&o.out.join(&f), &m64::write(&h, &pads))?;
        Some(f)
    } else {
        None
    };
    let meta = CycleMeta {
        cycle: c.n,
        m64: f,
        mupen64plus_m64: mp,
        frames: c.pads.len() as u32,
        first_height: c.blocks[0].height,
        last_height: c.blocks.last().unwrap().height,
        end: end.into(),
        blocks: c.blocks,
    };
    write(&o.out.join(format!("cycles/cycle_{:04}.json", c.n)), serde_json::to_string_pretty(&meta)?.as_bytes())?;
    Ok(meta)
}

/// Replays block `id` under `format`'s rules: the pads played on every frame and which were poked.
fn played_block(format: Format, id: &[u8; 32], parent: Parent, seed: &Seed, blob: &[u8]) -> Result<std::result::Result<sm64pow::chain::Played, BlockVerdict>> {
    match format {
        Format::Legacy => chain::played_wanted_block(id, parent, seed, blob),
        Format::V04 => Ok(match chain::played_committed_block(id, parent, seed, blob)? {
            Ok(p) => Ok(p),
            Err(chain::CommittedVerdict::Run(v)) => Err(v),
            Err(chain::CommittedVerdict::BadEncoding(why)) => Err(BlockVerdict::Invalid(format!("bad encoding: {why}"))),
        }),
    }
}

struct RecordCtx<'a> {
    block: &'a BlockRec,
    /// The block that carries the records (height + 1).
    next: &'a BlockRec,
    parent: Parent,
    kind: &'a str,
    block_start_hash: String,
    block_frames: u32,
    block_proof: &'a [u8],
    /// The previous block's end state (the natural start of a `parent`-start proof).
    parent_end: Option<&'a SaveState>,
    cycle: u32,
    /// The cycle's frames before the block.
    cycle_prefix: &'a [GamePad],
    first_ts: u64,
}

/// Exports the archived runs of `w.block`'s height: the records of `w.next`, each replayed from the
/// height's natural start with the consensus code, then plain-replayed with its effective pads.
fn export_records(o: &Options, w: &RecordCtx, log: &mut dyn FnMut(&str), sum: &mut Summary) -> Result<Vec<RecordMeta>> {
    use sm64pow::records;
    if w.next.records.is_empty() {
        return Ok(vec![]);
    }
    let recs = records::decode(&w.next.records).map_err(|e| anyhow!("block {}: its proofs field does not parse: {e}", w.next.height))?;
    if let Some(root) = w.next.proofs_root {
        let leaves: Vec<[u8; 32]> = recs.iter().map(|r| r.leaf().map_err(|e| anyhow!("{e}"))).collect::<Result<_>>()?;
        ensure!(records::root(&leaves) == root, "block {}: its records do not give the root its coinbase commits", w.next.height);
    }
    let winner_hdr = sm64pow::commit::header(w.block_proof).map_err(|e| anyhow!("block {}: {e}", w.block.height))?;
    // The weight window: within `weight_seconds` of auction time after the winner's due time.
    let weight_frames = match (w.block.difficulty, sm64pow::features::active().proof_weight, records::PROPOSED.weight_seconds) {
        (Some(d), true, Some(s)) => Some(records::window_frames(d, winner_hdr.frames, s.min(records::PROPOSED.archive_seconds))),
        _ => None,
    };
    let mut out = Vec::new();
    for (idx, r) in recs.iter().enumerate() {
        let f = r.facts().map_err(|e| anyhow!("block {} proof {idx}: {e}", w.next.height))?;
        let (start_kind, start_state) = match f.run.start {
            Start::PowerOn => ("power_on", fresh_game()?.save_state()),
            Start::Parent => match w.parent_end {
                Some(s) => ("parent", s.clone()),
                None => {
                    log(&format!("block {} proof {idx}: starts from the parent's end state, which is not in the input: skipped", w.next.height));
                    continue;
                }
            },
        };
        let id = f.leaf(&[0; 64]);
        let played = match chain::played_committed_block(&id, w.parent, &Seed(f.seed), &r.proof).with_context(|| format!("replaying proof {idx} of height {}", w.block.height))? {
            Ok(p) => p,
            Err(v) => {
                log(&format!("block {} proof {idx} does not replay: {v:?}: skipped", w.next.height));
                continue;
            }
        };
        let end = chain::cached(&id).ok_or_else(|| anyhow!("proof {idx} of height {}: end state not cached", w.block.height))?;
        let mut game = fresh_game()?;
        game.load_state(&start_state)?;
        let mut feats = Vec::new();
        play_plain(&mut game, &played.pads, if o.features { Some(&mut feats) } else { None })?;
        if game.save_state().as_bytes() != end.game.as_bytes() {
            log(&format!("MISMATCH proof {idx} of height {}: the plain replay of its effective pads ends in a different RAM image than the chain replay", w.block.height));
            sum.plain_replay_mismatches.push(w.block.height);
        }
        let gs = game.get_game_state()?;
        let frames = played.pads.len() as u32;
        let poked = played.poked.iter().filter(|p| **p).count() as u32;
        let name = format!("h{:07}_{:02}_{}", w.block.height, idx, hex::encode(&f.run.input_hash[..4]));
        let mut files = RecordFiles { meta: format!("records/{name}.json"), pads: format!("records/{name}.pads"), ..Default::default() };
        write(&o.out.join(&files.pads), &m64::frames_bytes(&played.pads))?;
        if o.features {
            files.features = Some(format!("records/{name}.features.bin"));
            write(&o.out.join(files.features.as_ref().unwrap()), &feats)?;
        }
        // A movie from power-on, when the start is one a cycle prefix reaches.
        let mut m64_frame_offset = None;
        if o.record_m64 {
            let prefix: Option<&[GamePad]> = match (f.run.start, w.kind) {
                (Start::PowerOn, "genesis" | "cycle_start" | "power_on") => Some(&[]),
                (Start::Parent, "parent") => Some(w.cycle_prefix),
                _ => None,
            };
            if let Some(prefix) = prefix {
                let mut pads = prefix.to_vec();
                pads.extend_from_slice(&played.pads);
                if o.clear_reserved_bits {
                    pads = m64::clear_reserved_bits(&pads);
                }
                let desc = format!("SM64Coin height {} proof {idx} (miner key {}): effective inputs, pokes applied", w.block.height, hex::encode(&f.key[..4]));
                let h = Header::power_on(0, w.first_ts as u32, o.rom.clone(), &o.author, &desc);
                let file = format!("records/{name}.m64");
                write(&o.out.join(&file), &m64::write(&h, &pads))?;
                files.m64 = Some(file);
                m64_frame_offset = Some(prefix.len() as u32);
            }
        }
        let (ev, course, star) = match end.event {
            Some(e) => match e {
                sm64pow::game::Event::Star { course, star } => (Some(e.name()), Some(course), Some(star)),
                _ => (Some(e.name()), None, None),
            },
            None => (None, None, None),
        };
        let counted = weight_frames.map(|wf| f.run.frames <= wf);
        let meta = RecordMeta {
            height: w.block.height,
            index: idx as u32,
            included_in_height: w.next.height,
            included_in_id: hex::encode(w.next.id),
            input_hash: hex::encode(f.run.input_hash),
            seed: hex::encode(f.seed),
            miner_key: hex::encode(f.key),
            payout: hex::encode(crate::source::payout_bytes(&f.payout)),
            sig: hex::encode(r.sig),
            proof_codec: sm64pow::commit::split(&r.proof).ok().and_then(|(_, enc)| enc.first().copied()),
            proof_bytes: r.proof.len(),
            frames,
            poked_frames: poked,
            poked_mask_hex: pack_mask(&played.poked),
            start_kind: start_kind.into(),
            start_state_hash: hash(&start_state),
            start_same_as_block: hash(&start_state) == w.block_start_hash,
            counted_for_weight: counted,
            frames_vs_winner: frames as i64 - w.block_frames as i64,
            progress_after: end.progress.into(),
            event: ev,
            course,
            star,
            end_course_num: gs.course_num,
            end_level_num: gs.level_num,
            end_area_index: gs.area_index,
            end_act_num: gs.act_num,
            end_pos: gs.pos,
            end_state_hash: hash(&end.game),
            cycle: w.cycle,
            m64_frame_offset,
            files,
        };
        write(&o.out.join(&meta.files.meta), serde_json::to_string_pretty(&meta)?.as_bytes())?;
        sum.records += 1;
        sum.record_frames += frames as u64;
        sum.records_counted += counted.unwrap_or(false) as usize;
        log(&format!("  height {} proof {idx}: {} frames ({} poked, {:+} vs the block) key {} counted {:?}", w.block.height, frames, poked, meta.frames_vs_winner, hex::encode(&f.key[..4]), counted));
        out.push(meta);
    }
    Ok(out)
}
