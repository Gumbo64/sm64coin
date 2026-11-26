//! `sm64coin-export`: chain -> m64 / segments / features. Usage in docs/EXPORT.md.

use anyhow::{Result, anyhow, bail};
use sm64coin_export::export::{Format, Options, States, export};
use sm64coin_export::m64::Rom;
use sm64coin_export::{sampler, snapshot, source, verify};
use std::path::PathBuf;

const USAGE: &str = "usage:
  sm64coin-export compare-mupen-dump CYCLE.m64 DUMP.bin [pool]   (a Mupen64Plus per-poll RAM dump, tools/mupen64plus/threeway.sh)
  sm64coin-export export --out DIR (--jsonl FILE | --rpc URL [--from H] [--to H]) [--rom PATH]
                  [--features] [--states none|portable|raw|both] [--mupen64plus] [--clear-reserved-bits] [--max-blocks N]
                  [--format v0.4|legacy] [--record-m64]
  sm64coin-export snapshot --out DIR [--tar FILE.tar.gz] (same source and options as export; states default to portable)
  sm64coin-export verify-snapshot DIR|FILE.tar.gz [--no-replay] [--mupen auto|WORKDIR] [--mupen-records N] [--mupen-tools DIR] [--report FILE]
  sm64coin-export sample EXPORT_DIR --out DIR [--samplers uniform,objective,random,jitter] [--states N] [--branches K]
                  [--horizon H] [--stride S] [--seed X] [--from-height H] [--to-height H] [--eps E] [--keep-cutscenes]   (docs/SAMPLER.md)
  sm64coin-export verify DIR [--cycle N] [--rom PATH]
  sm64coin-export fetch --rpc URL --out FILE.jsonl [--from H] [--to H]
The ROM defaults to $SM64COIN_ROM, ./baserom.us.z64 or ~/.sm64coin/baserom.us.z64.";

fn format(v: Option<&str>) -> Result<Format> {
    match v {
        None | Some("v0.4") | Some("v04") => Ok(Format::V04),
        Some("legacy") | Some("v0.3") | Some("v0.2") => Ok(Format::Legacy),
        Some(x) => bail!("--format {x}? (v0.4 or legacy)"),
    }
}

/// The blocks the command works on (`--jsonl FILE` or `--rpc URL`, `--from`, `--to`, `--max-blocks`) and what
/// is known about the chain they came from.
fn load_blocks(args: &[String]) -> Result<(Vec<source::BlockRec>, snapshot::ChainInfo)> {
    let val = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let mut chain = snapshot::ChainInfo { network: "unknown".into(), ..Default::default() };
    let mut blocks = if let Some(f) = val("--jsonl") {
        let mut v = source::read_jsonl(&std::fs::read_to_string(f)?)?;
        let from = val("--from").map(|x| x.parse::<u64>()).transpose()?.unwrap_or(0);
        let to = val("--to").map(|x| x.parse::<u64>()).transpose()?.unwrap_or(u64::MAX);
        v.retain(|b| b.height >= from && b.height <= to);
        v
    } else {
        let url = val("--rpc").ok_or_else(|| anyhow!("{USAGE}"))?;
        let rpc = source::Rpc::new(&url);
        let info = rpc.info()?;
        chain.node = Some(url);
        chain.network = info["nettype"].as_str().unwrap_or("unknown").to_string();
        chain.node_height = info["height"].as_u64();
        chain.node_top_block = info["top_block_hash"].as_str().map(String::from);
        chain.genesis_id = rpc.block_hash(0).ok().map(hex::encode);
        let from = val("--from").map(|x| x.parse::<u64>()).transpose()?.unwrap_or(1);
        let to = match val("--to") { Some(t) => t.parse::<u64>()?, None => rpc.height()?.saturating_sub(1) };
        (from..=to).map(|h| rpc.block(h)).collect::<Result<Vec<_>>>()?
    };
    if let Some(n) = val("--max-blocks") {
        blocks.truncate(n.parse()?);
    }
    if chain.genesis_id.is_none() {
        chain.genesis_id = blocks.first().filter(|b| b.height == 1).map(|b| hex::encode(b.prev));
    }
    Ok((blocks, chain))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| args.iter().any(|a| a == n);
    let val = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let Some(cmd) = args.first() else { bail!("{USAGE}") };
    let rom_path = val("--rom").or_else(sm64pow::default_rom_path).ok_or_else(|| anyhow!("no ROM: pass --rom or set SM64COIN_ROM"));
    let mut log = |s: &str| eprintln!("{s}");
    match cmd.as_str() {
        "fetch" | "export" | "snapshot" => {
            let (blocks, chain) = load_blocks(&args)?;
            if cmd == "fetch" {
                let text: String = blocks.iter().map(|b| source::to_line(b) + "\n").collect();
                std::fs::write(val("--out").ok_or_else(|| anyhow!("{USAGE}"))?, text)?;
                return Ok(());
            }
            let rom = std::fs::read(rom_path?)?;
            let info = Rom::from_z64(&rom)?;
            sm64pow::init(rom)?;
            let states = match val("--states").as_deref() {
                None => if cmd == "snapshot" { States::Portable } else { States::None },
                Some("none") => States::None,
                Some("portable") => States::Portable,
                Some("raw") => States::Raw,
                Some("both") => States::Both,
                Some(x) => bail!("--states {x}?"),
            };
            let out = PathBuf::from(val("--out").ok_or_else(|| anyhow!("{USAGE}"))?);
            let snapshot = cmd == "snapshot";
            let o = Options { out: out.clone(), rom: info, features: flag("--features"), states, mupen64plus: flag("--mupen64plus"), clear_reserved_bits: flag("--clear-reserved-bits"), format: format(val("--format").as_deref())?, record_m64: flag("--record-m64"), author: "sm64coin-export".into() };
            let s = export(&blocks, &o, &mut log)?;
            println!("{} blocks, {} cycles, {} frames ({} poked); {} archived proofs ({} frames, {} counted for weight); plain-replay mismatches {:?}, portable mismatches {:?}", s.blocks, s.cycles, s.frames, s.poked, s.records, s.record_frames, s.records_counted, s.plain_replay_mismatches, s.portable_mismatches);
            if snapshot {
                let created = std::env::var("SOURCE_DATE_EPOCH").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
                let snap = snapshot::finish(&out, &chain, created)?;
                println!("snapshot {}: {} files, {} bytes, sums {}", out.display(), snap["counts"]["files"], snap["counts"]["bytes"], snap["sums_sha256"]);
                if let Some(t) = val("--tar") {
                    let top = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "snapshot".into());
                    snapshot::tar_gz(&out, std::path::Path::new(&t), &top)?;
                    println!("archive {t}: {} bytes", std::fs::metadata(&t)?.len());
                }
            }
            if !s.plain_replay_mismatches.is_empty() || !s.portable_mismatches.is_empty() {
                std::process::exit(2);
            }
        }
        "verify-snapshot" => {
            let target = PathBuf::from(args.get(1).ok_or_else(|| anyhow!("{USAGE}"))?);
            let tmp;
            let dir = if target.is_dir() {
                target
            } else {
                tmp = val("--work").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join(format!("sm64coin-verify-{}", std::process::id()))).join("unpacked");
                snapshot::untar(&target, &tmp)?
            };
            let norep = flag("--no-replay");
            if !norep {
                sm64pow::init(std::fs::read(rom_path.as_ref().map_err(|e| anyhow!("{e}"))?)?)?;
            }
            let vo = snapshot::VerifyOptions {
                replay: !norep,
                mupen_work: match val("--mupen").as_deref() { None | Some("off") => None, Some("auto") => Some(std::env::temp_dir().join(format!("sm64coin-mupen-{}", std::process::id()))), Some(d) => Some(PathBuf::from(d)) },
                mupen_records: val("--mupen-records").map(|n| n.parse()).transpose()?.unwrap_or(0),
                mupen_tools: val("--mupen-tools").map(PathBuf::from),
                rom: val("--rom").map(PathBuf::from),
            };
            let r = snapshot::verify(&dir, &vo, &mut log)?;
            println!("snapshot {}: {} files, {} blocks, {} archived proofs, {} frames replayed; three-way: {}", if r.ok() { "OK" } else { "FAILED" }, r.files, r.blocks, r.records, r.frames, if r.mupen.is_empty() { "not requested".to_string() } else { r.mupen.join(" | ") });
            for p in &r.problems {
                println!("PROBLEM {p}");
            }
            if let Some(f) = val("--report") {
                std::fs::write(f, serde_json::to_string_pretty(&serde_json::json!({"ok": r.ok(), "files": r.files, "blocks": r.blocks, "records": r.records, "frames": r.frames, "problems": r.problems, "three_way": r.mupen}))?)?;
            }
            if !r.ok() {
                std::process::exit(2);
            }
        }
        "sample" => {
            // sample EXPORT_DIR --out DIR [...]: counterfactual rollouts from states on the chain (docs/SAMPLER.md).
            let dir = PathBuf::from(args.get(1).ok_or_else(|| anyhow!("{USAGE}"))?);
            sm64pow::init(std::fs::read(rom_path?)?)?;
            let d = sampler::Options::default();
            let num = |n: &str, dflt: usize| -> Result<usize> { Ok(val(n).map(|x| x.parse()).transpose()?.unwrap_or(dflt)) };
            let o = sampler::Options {
                out: PathBuf::from(val("--out").ok_or_else(|| anyhow!("{USAGE}"))?),
                samplers: match val("--samplers") { Some(l) => l.split(',').map(sampler::Kind::parse).collect::<Result<Vec<_>>>()?, None => d.samplers },
                states: num("--states", d.states)?,
                branches: num("--branches", d.branches)?,
                horizon: num("--horizon", d.horizon)?,
                stride: num("--stride", d.stride)?,
                seed: val("--seed").map(|x| x.parse()).transpose()?.unwrap_or(d.seed),
                heights: match (val("--from-height"), val("--to-height")) { (None, None) => None, (a, b) => Some((a.map(|x| x.parse()).transpose()?.unwrap_or(0), b.map(|x| x.parse()).transpose()?.unwrap_or(u64::MAX))) },
                eps: val("--eps").map(|x| x.parse()).transpose()?.unwrap_or(d.eps),
                skip_cutscenes: !flag("--keep-cutscenes"),
            };
            let s = sampler::sample(&dir, &o, &mut log)?;
            println!("{} candidate start states, {} cells on the chain", s.candidates, s.chain_cells);
            for (k, t, f, c) in &s.samplers {
                println!("{:<10} {t} trajectories, {f} frames, {c} cells", k.name());
            }
        }
        "lead-poll" => {
            // lead-poll IN.m64 OUT.m64: the movie with one leading zero poll (what Mupen64Plus needs).
            let (h, pads) = sm64coin_export::m64::read(&std::fs::read(args.get(1).ok_or_else(|| anyhow!("{USAGE}"))?)?)?;
            let mut p = vec![sm64_binds::GamePad::default()];
            p.extend(pads);
            std::fs::write(args.get(2).ok_or_else(|| anyhow!("{USAGE}"))?, sm64coin_export::m64::write(&h, &p))?;
        }
        "m64-frames" => {
            let (_, pads) = sm64coin_export::m64::read(&std::fs::read(args.get(1).ok_or_else(|| anyhow!("{USAGE}"))?)?)?;
            println!("{}", pads.len());
        }
        "compare-mupen-dump" => {
            // compare-mupen-dump CYCLE.m64 DUMP.bin [pool]: every poll of a Mupen64Plus RDRAM dump against the port.
            sm64pow::init(std::fs::read(rom_path?)?)?;
            let (_, pads) = sm64coin_export::m64::read(&std::fs::read(args.get(1).ok_or_else(|| anyhow!("{USAGE}"))?)?)?;
            let dump = std::fs::read(args.get(2).ok_or_else(|| anyhow!("{USAGE}"))?)?;
            let r = sm64coin_export::mupen::compare_dump(&pads, &dump, args.get(3).map(String::as_str) == Some("pool"))?;
            for (poll, frames, what) in &r.first {
                println!("poll {poll} (port after {frames} frames): {what}");
            }
            println!("{} polls compared, {} differ", r.polls, r.differing);
            if r.differing > 0 {
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
            println!("verified {} cycles, {} blocks, {} archived proofs, {} frames: {} divergences", r.cycles, r.blocks, r.records, r.frames, r.divergences.len());
            if !r.divergences.is_empty() {
                std::process::exit(2);
            }
        }
        _ => bail!("{USAGE}"),
    }
    Ok(())
}
