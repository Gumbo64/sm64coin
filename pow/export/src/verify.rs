//! Verifying an export from its files alone: replay every cycle's m64 from
//! power-on in the headless game, WITHOUT pokes, and compare the whole RAM
//! image at every block boundary with the chain replay's (the hashes the
//! export recorded). Also checks that each block's `.pads` file is the right
//! slice of its cycle's m64.

use crate::m64;
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Default)]
pub struct Report {
    pub cycles: usize,
    pub blocks: usize,
    /// v0.4 archived proofs replayed (each from its height's start, no pokes).
    pub records: usize,
    pub frames: u64,
    pub divergences: Vec<String>,
}

pub fn verify(dir: &Path, only_cycle: Option<u32>, log: &mut dyn FnMut(&str)) -> Result<Report> {
    let manifest: Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json"))?)?;
    let w = crate::walk::walk(dir, only_cycle, log, &mut |_| Ok(()))?;
    let mut rep = Report { cycles: w.cycles, blocks: w.blocks, records: w.records, frames: w.frames, divergences: w.divergences };
    let mut cycle_pads = std::collections::HashMap::new();
    for c in manifest["cycles"].as_array().context("manifest.cycles")? {
        let n = c["cycle"].as_u64().unwrap() as u32;
        if only_cycle.is_some_and(|o| o != n) {
            continue;
        }
        cycle_pads.insert(n, m64::read(&std::fs::read(dir.join(c["m64"].as_str().unwrap()))?)?.1);
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
        // The movie may have the reserved button bits cleared (`--clear-reserved-bits`), the .pads file never does.
        if seg != pads[off..off + frames] && m64::clear_reserved_bits(&seg) != pads[off..off + frames] {
            let msg = format!("block {}: its .pads file is not the cycle's slice", m["height"]);
            log(&format!("DIVERGENCE {msg}"));
            rep.divergences.push(msg);
        }
    }
    Ok(rep)
}
