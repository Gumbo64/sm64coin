//! Walking an export from its files alone: the segments of the chain (every block and, from v0.4,
//! every archived proof of a height) with their start states, reconstructed by replaying each cycle's
//! m64 from power-on WITHOUT pokes. `verify` and the branching sampler are built on it.
//!
//! A block segment starts where the cycle replay is at the block's first frame. A proof segment
//! starts from its height's natural start: power-on, or the end state of the previous block (which
//! is the state just before the height's block, or, after a restart, the end of an abandoned cycle:
//! the walk keeps the last block's end state across cycles). Every start state's hash and every end
//! state's hash is compared with the one the export recorded.

use crate::export::{fresh_game, hash};
use crate::m64;
use anyhow::{Context, Result, anyhow, ensure};
use serde_json::Value;
use sm64_binds::{GamePad, SaveState};
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegKind {
    Block,
    /// An archived run of a height (v0.4 included proof).
    Record,
}

/// One segment: a start state and the pads that were played from it.
pub struct Seg {
    pub kind: SegKind,
    pub height: u64,
    /// File stem (`h0000012_ab12cd34`, `h0000012_03_ab12cd34`).
    pub name: String,
    pub cycle: u32,
    pub start: SaveState,
    pub start_hash: String,
    pub end_hash: String,
    pub pads: Vec<GamePad>,
    /// Whether each frame was a poke (the perturbation's pad, not the player's).
    pub poked: Vec<bool>,
    pub meta: Value,
}

#[derive(Debug, Default)]
pub struct Report {
    pub cycles: usize,
    pub blocks: usize,
    pub records: usize,
    pub frames: u64,
    pub divergences: Vec<String>,
}

fn read_json(p: &Path) -> Result<Value> {
    serde_json::from_slice(&std::fs::read(p).with_context(|| format!("reading {}", p.display()))?).with_context(|| format!("parsing {}", p.display()))
}

fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().ok_or_else(|| anyhow!("missing {k}"))
}

fn mask(h: &str, frames: usize) -> Result<Vec<bool>> {
    crate::export::unpack_mask(h, frames)
}

/// Replays `dir` and calls `f` for every segment, in chain order (a height's block, then its proofs).
/// Divergences from the recorded hashes are reported (and logged), not fatal.
pub fn walk(dir: &Path, only_cycle: Option<u32>, log: &mut dyn FnMut(&str), f: &mut dyn FnMut(&Seg) -> Result<()>) -> Result<Report> {
    let manifest = read_json(&dir.join("manifest.json"))?;
    let mut rep = Report::default();
    // Metadata by height.
    let mut block_meta: HashMap<u64, Value> = HashMap::new();
    for e in std::fs::read_dir(dir.join("blocks"))? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "json") {
            let m = read_json(&p)?;
            block_meta.insert(m["height"].as_u64().context("height")?, m);
        }
    }
    let mut records: HashMap<u64, Vec<Value>> = HashMap::new();
    for rf in manifest["record_files"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        let m = read_json(&dir.join(rf.as_str().context("record file")?))?;
        records.entry(m["height"].as_u64().context("record height")?).or_default().push(m);
    }
    let bad = |rep: &mut Report, log: &mut dyn FnMut(&str), msg: String| {
        log(&format!("DIVERGENCE {msg}"));
        rep.divergences.push(msg);
    };
    // The previous block's end state (height, state): the natural start of the next height.
    let mut prev_end: Option<(u64, SaveState)> = None;
    for c in manifest["cycles"].as_array().context("manifest.cycles")? {
        let n = c["cycle"].as_u64().unwrap() as u32;
        if only_cycle.is_some_and(|o| o != n) {
            continue;
        }
        let (h, pads) = m64::read(&std::fs::read(dir.join(s(c, "m64")?))?)?;
        ensure!(h.start_type == 2, "cycle {n}: not a power-on movie");
        ensure!(pads.len() as u64 == c["frames"].as_u64().unwrap(), "cycle {n}: frame count");
        let mut g = fresh_game()?;
        let mut at = 0usize;
        for b in c["blocks"].as_array().unwrap() {
            let height = b["height"].as_u64().unwrap();
            let end = b["end_frame"].as_u64().unwrap() as usize;
            let start = g.save_state();
            let start_hash = hash(&start);
            let meta = block_meta.get(&height).with_context(|| format!("no metadata for block {height}"))?;
            if start_hash != s(meta, "start_state_hash")? {
                bad(&mut rep, log, format!("cycle {n} block {height}: the start state differs from the recorded one"));
            }
            // Proofs of this height start from the natural start, before the block's frames are played.
            for r in records.get(&height).map(|v| v.as_slice()).unwrap_or(&[]) {
                let rpads = m64::frames_from_bytes(&std::fs::read(dir.join(s(&r["files"], "pads")?))?)?;
                let rstart = match s(r, "start_kind")? {
                    "power_on" => Some(fresh_game()?.save_state()),
                    _ => prev_end.as_ref().filter(|p| p.0 + 1 == height).map(|p| p.1.clone()),
                };
                let name = std::path::Path::new(s(&r["files"], "meta")?).file_stem().and_then(|x| x.to_str()).unwrap_or("").to_string();
                let Some(rstart) = rstart else {
                    log(&format!("height {height} proof {name}: its start state is not reachable from the exported cycles: skipped"));
                    continue;
                };
                let rstart_hash = hash(&rstart);
                if rstart_hash != s(r, "start_state_hash")? {
                    bad(&mut rep, log, format!("height {height} proof {name}: the start state differs from the recorded one"));
                }
                let mut rg = fresh_game()?;
                rg.load_state(&rstart)?;
                for p in &rpads {
                    rg.step_game(*p)?;
                }
                let rend = hash(&rg.save_state());
                if rend != s(r, "end_state_hash")? {
                    bad(&mut rep, log, format!("height {height} proof {name}: the state after its {} frames differs from the chain replay", rpads.len()));
                }
                // The proof's m64 (from power-on) must be the cycle's frames up to the block, then the proof's.
                if let Some(mf) = r["files"]["m64"].as_str() {
                    let (_, mp) = m64::read(&std::fs::read(dir.join(mf))?)?;
                    let off = r["m64_frame_offset"].as_u64().context("m64_frame_offset")? as usize;
                    let same = mp.len() == off + rpads.len() && mp[off..] == rpads[..] && (off == 0 || (off <= at && mp[..off] == pads[..off]));
                    // (clear-reserved-bits movies differ from the raw pads only in the reserved bits)
                    let same = same || (mp.len() == off + rpads.len() && m64::clear_reserved_bits(&rpads) == mp[off..]);
                    if !same {
                        bad(&mut rep, log, format!("height {height} proof {name}: its m64 is not the cycle prefix plus its pads"));
                    }
                }
                let poked = mask(s(r, "poked_mask_hex")?, rpads.len())?;
                let seg = Seg { kind: SegKind::Record, height, name, cycle: n, start: rstart, start_hash: rstart_hash, end_hash: rend, pads: rpads.clone(), poked, meta: r.clone() };
                f(&seg)?;
                rep.records += 1;
                rep.frames += rpads.len() as u64;
            }
            // The block itself.
            for p in &pads[at..end] {
                g.step_game(*p)?;
            }
            let end_state = g.save_state();
            let got = hash(&end_state);
            if got != s(b, "end_state_hash")? {
                bad(&mut rep, log, format!("cycle {n} block {height} (frame {end}): state differs from the chain replay"));
            }
            let seg = Seg {
                kind: SegKind::Block,
                height,
                name: std::path::Path::new(s(&meta["files"], "pads")?).file_stem().and_then(|x| x.to_str()).unwrap_or("").to_string(),
                cycle: n,
                start,
                start_hash,
                end_hash: got,
                pads: pads[at..end].to_vec(),
                poked: mask(s(meta, "poked_mask_hex")?, end - at)?,
                meta: meta.clone(),
            };
            f(&seg)?;
            at = end;
            prev_end = Some((height, end_state));
            rep.blocks += 1;
        }
        log(&format!("cycle {n}: {} frames, {} blocks replayed from power-on without pokes", pads.len(), c["blocks"].as_array().unwrap().len()));
        rep.frames += pads.len() as u64;
        rep.cycles += 1;
    }
    // Block `.pads` files are the cycle's slices (checked by `verify`).
    Ok(rep)
}
