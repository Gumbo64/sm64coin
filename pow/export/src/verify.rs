//! Verifying an export from its files alone: replay every cycle's m64 from
//! power-on in the headless game, WITHOUT pokes, and compare the whole RAM
//! image at every block boundary with the chain replay's (the hashes the
//! export recorded). Also checks that each block's `.pads` file is the right
//! slice of its cycle's m64.

use crate::export::{fresh_game, hash};
use crate::m64;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Default)]
pub struct Report {
    pub cycles: usize,
    pub blocks: usize,
    pub frames: u64,
    pub divergences: Vec<String>,
}

pub fn verify(dir: &Path, only_cycle: Option<u32>, log: &mut dyn FnMut(&str)) -> Result<Report> {
    let manifest: Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json"))?)?;
    let mut rep = Report::default();
    let mut cycle_pads = std::collections::HashMap::new();
    for c in manifest["cycles"].as_array().context("manifest.cycles")? {
        let n = c["cycle"].as_u64().unwrap() as u32;
        if only_cycle.is_some_and(|o| o != n) {
            continue;
        }
        let (h, pads) = m64::read(&std::fs::read(dir.join(c["m64"].as_str().unwrap()))?)?;
        ensure!(h.start_type == 2, "cycle {n}: not a power-on movie");
        ensure!(pads.len() as u64 == c["frames"].as_u64().unwrap(), "cycle {n}: frame count");
        let mut g = fresh_game()?;
        let mut at = 0usize;
        for b in c["blocks"].as_array().unwrap() {
            let end = b["end_frame"].as_u64().unwrap() as usize;
            for p in &pads[at..end] {
                g.step_game(*p)?;
            }
            at = end;
            let got = hash(&g.save_state());
            let height = b["height"].as_u64().unwrap();
            if got != b["end_state_hash"].as_str().unwrap() {
                let msg = format!("cycle {n} block {height} (frame {end}): state differs from the chain replay");
                log(&format!("DIVERGENCE {msg}"));
                rep.divergences.push(msg);
            }
            rep.blocks += 1;
        }
        log(&format!("cycle {n}: {} frames, {} blocks replayed from power-on without pokes", pads.len(), c["blocks"].as_array().unwrap().len()));
        rep.frames += pads.len() as u64;
        rep.cycles += 1;
        cycle_pads.insert(n, pads);
    }
    for e in std::fs::read_dir(dir.join("blocks"))? {
        let p = e?.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let m: Value = serde_json::from_slice(&std::fs::read(&p)?)?;
        let n = m["cycle"].as_u64().unwrap() as u32;
        let Some(pads) = cycle_pads.get(&n) else { continue };
        let off = m["cycle_frame_offset"].as_u64().unwrap() as usize;
        let frames = m["frames"].as_u64().unwrap() as usize;
        let seg = m64::frames_from_bytes(&std::fs::read(dir.join(m["files"]["pads"].as_str().unwrap()))?)?;
        if seg != pads[off..off + frames] {
            let msg = format!("block {}: its .pads file is not the cycle's slice", m["height"]);
            log(&format!("DIVERGENCE {msg}"));
            rep.divergences.push(msg);
        }
    }
    Ok(rep)
}
