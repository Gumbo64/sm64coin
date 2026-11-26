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
    pub author: String,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Prog {
    pub stars: u32,
    pub keys: u32,
    pub grand_star: bool,
    pub points: u32,
}

impl From<Progress> for Prog {
    fn from(p: Progress) -> Prog {
        Prog { stars: p.stars, keys: p.keys, grand_star: p.grand_star, points: p.points() }
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
    pub files: BlockFiles,
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
    pub plain_replay_mismatches: Vec<u64>,
    pub portable_mismatches: Vec<u64>,
}

pub fn export(blocks: &[BlockRec], o: &Options, log: &mut dyn FnMut(&str)) -> Result<Summary> {
    ensure!(!blocks.is_empty(), "no blocks");
    for d in ["blocks", "cycles"] {
        std::fs::create_dir_all(o.out.join(d))?;
    }
    write(&o.out.join("features.schema.json"), serde_json::to_string_pretty(&features::schema())?.as_bytes())?;

    let mut sum = Summary { blocks: 0, cycles: 0, frames: 0, poked: 0, plain_replay_mismatches: vec![], portable_mismatches: vec![] };
    let mut cycles_meta: Vec<CycleMeta> = Vec::new();
    let mut cur: Option<Cycle> = None;
    // The previous block: its end state, id, timestamp, progress, whether it ended the cycle.
    let mut prev: Option<(u64, [u8; 32], u64, SaveState, Progress, bool)> = None;
    let mut n_cycles = 0u32;

    for b in blocks {
        if let Some((h, id, ..)) = &prev {
            ensure!(b.height == h + 1 && b.prev == *id, "block {} does not extend block {h}: the input must be one chain in height order", b.height);
        }
        let start = proof::proof_start(&b.proof).map_err(|e| anyhow!("block {}: {e}", b.height))?;
        let (_, _, codec, _) = proof::split_blob(&b.proof)?;
        let parent = if b.height == 1 { Parent::Genesis } else { Parent::Block { id: b.prev, gap_seconds: prev.as_ref().map(|p| b.ts.saturating_sub(p.2)).unwrap_or(0) } };
        let played = match chain::played_wanted_block(&b.id, parent, &Seed(b.seed), &b.proof)
            .with_context(|| format!("replaying block {}", b.height))?
        {
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
        "game_build": sm64pow::GAME_BUILD,
        "rom": {"name": String::from_utf8_lossy(&o.rom.name), "crc1": format!("{:08x}", o.rom.crc1)},
        "blocks": sum.blocks, "frames": sum.frames, "poked_frames": sum.poked,
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
