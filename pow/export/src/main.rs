//! `sm64coin-export`: chain -> m64 / segments / features. Usage in docs/EXPORT.md.

use anyhow::{Result, anyhow, bail};
use sm64coin_export::export::{Options, States, export};
use sm64coin_export::m64::Rom;
use sm64coin_export::{source, verify};
use std::path::PathBuf;

const USAGE: &str = "usage:
  sm64coin-export export --out DIR (--jsonl FILE | --rpc URL [--from H] [--to H]) [--rom PATH]
                  [--features] [--states none|portable|raw|both] [--mupen64plus] [--max-blocks N]
  sm64coin-export verify DIR [--cycle N] [--rom PATH]
  sm64coin-export fetch --rpc URL --out FILE.jsonl [--from H] [--to H]
The ROM defaults to $SM64COIN_ROM, ./baserom.us.z64 or ~/.sm64coin/baserom.us.z64.";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| args.iter().any(|a| a == n);
    let val = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let Some(cmd) = args.first() else { bail!("{USAGE}") };
    let rom_path = val("--rom").or_else(sm64pow::default_rom_path).ok_or_else(|| anyhow!("no ROM: pass --rom or set SM64COIN_ROM"));
    let mut log = |s: &str| eprintln!("{s}");
    match cmd.as_str() {
        "fetch" | "export" => {
            let blocks = if let Some(f) = val("--jsonl") {
                let mut v = source::read_jsonl(&std::fs::read_to_string(f)?)?;
                let from = val("--from").map(|x| x.parse::<u64>()).transpose()?.unwrap_or(0);
                let to = val("--to").map(|x| x.parse::<u64>()).transpose()?.unwrap_or(u64::MAX);
                v.retain(|b| b.height >= from && b.height <= to);
                v
            } else {
                let rpc = source::Rpc::new(&val("--rpc").ok_or_else(|| anyhow!("{USAGE}"))?);
                let from = val("--from").map(|x| x.parse::<u64>()).transpose()?.unwrap_or(1);
                let to = match val("--to") { Some(t) => t.parse::<u64>()?, None => rpc.height()?.saturating_sub(1) };
                (from..=to).map(|h| rpc.block(h)).collect::<Result<Vec<_>>>()?
            };
            let mut blocks = blocks;
            if let Some(n) = val("--max-blocks") {
                blocks.truncate(n.parse()?);
            }
            if cmd == "fetch" {
                let text: String = blocks.iter().map(|b| source::to_line(b) + "\n").collect();
                std::fs::write(val("--out").ok_or_else(|| anyhow!("{USAGE}"))?, text)?;
                return Ok(());
            }
            let rom = std::fs::read(rom_path?)?;
            let info = Rom::from_z64(&rom)?;
            sm64pow::init(rom)?;
            let states = match val("--states").as_deref() {
                None | Some("none") => States::None,
                Some("portable") => States::Portable,
                Some("raw") => States::Raw,
                Some("both") => States::Both,
                Some(x) => bail!("--states {x}?"),
            };
            let o = Options { out: PathBuf::from(val("--out").ok_or_else(|| anyhow!("{USAGE}"))?), rom: info, features: flag("--features"), states, mupen64plus: flag("--mupen64plus"), author: "sm64coin-export".into() };
            let s = export(&blocks, &o, &mut log)?;
            println!("{} blocks, {} cycles, {} frames ({} poked); plain-replay mismatches {:?}, portable mismatches {:?}", s.blocks, s.cycles, s.frames, s.poked, s.plain_replay_mismatches, s.portable_mismatches);
            if !s.plain_replay_mismatches.is_empty() || !s.portable_mismatches.is_empty() {
                std::process::exit(2);
            }
        }
        "compare-mupen" => {
            // compare-mupen CYCLE.m64 LEAD STATE.st...: each state is `after_<poll>.st` saved by tools/mupen64plus.
            // LEAD is 1 for the .mupen64plus.m64 variant (one leading zero poll), else 0.
            sm64pow::init(std::fs::read(rom_path?)?)?;
            let (_, pads) = sm64coin_export::m64::read(&std::fs::read(args.get(1).ok_or_else(|| anyhow!("{USAGE}"))?)?)?;
            let lead: usize = args.get(2).ok_or_else(|| anyhow!("{USAGE}"))?.parse()?;
            let mut bad = 0;
            let mut files: Vec<&String> = args[3..].iter().collect();
            files.sort_by_key(|f| std::path::Path::new(f.as_str()).file_stem().and_then(|s| s.to_str()).and_then(|s| s.strip_prefix("after_")).and_then(|n| n.parse::<usize>().ok()));
            for f in files {
                let poll: usize = std::path::Path::new(f).file_stem().and_then(|s| s.to_str()).and_then(|s| s.strip_prefix("after_")).ok_or_else(|| anyhow!("{f}: name it after_<poll>.st"))?.parse()?;
                let st = match sm64_binds::mupen::MupenState::from_file(f) { Ok(s) => s, Err(e) => { bad += 1; println!("{f}: unreadable ({e})"); continue } };
                let m = sm64coin_export::mupen::from_state(&st)?;
                // The port after `poll - lead` frames, and its neighbours (to show an offset if there is one).
                let frames = poll - lead - 1; // a state requested in poll p is the port after p - lead - 1 frames (docs/EXPORT.md)
                let mut g = sm64coin_export::export::fresh_game()?;
                let mut hits = Vec::new();
                for i in 0..=pads.len().min(frames + 2) {
                    if i + 2 >= frames && sm64coin_export::mupen::same_mario(&m, &sm64coin_export::mupen::from_port(&g)?) {
                        hits.push(i);
                    }
                    if i < pads.len() {
                        g.step_game(pads[i])?;
                    }
                }
                if hits.contains(&frames) {
                    println!("{f}: Mupen64Plus == port after {frames} frames (exact)");
                } else if let Some(i) = hits.first() {
                    bad += 1;
                    println!("{f}: Mupen64Plus matches the port after {i} frames, expected {frames}");
                } else {
                    bad += 1;
                    let mut g = sm64coin_export::export::fresh_game()?;
                    for p in &pads[..frames.min(pads.len())] { g.step_game(*p)?; }
                    let s = sm64coin_export::mupen::from_port(&g)?;
                    println!("{f}: DIVERGES at frame {frames}: Mupen64Plus {m:?} port {s:?}");
                }
            }
            if bad > 0 { std::process::exit(2); }
        }
        "verify" => {
            sm64pow::init(std::fs::read(rom_path?)?)?;
            let dir = args.get(1).ok_or_else(|| anyhow!("{USAGE}"))?;
            let r = verify::verify(std::path::Path::new(dir), val("--cycle").map(|c| c.parse()).transpose()?, &mut log)?;
            println!("verified {} cycles, {} blocks, {} frames: {} divergences", r.cycles, r.blocks, r.frames, r.divergences.len());
            if !r.divergences.is_empty() {
                std::process::exit(2);
            }
        }
        _ => bail!("{USAGE}"),
    }
    Ok(())
}
