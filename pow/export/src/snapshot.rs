//! Dataset snapshots: an export directory plus a versioned manifest (`snapshot.json`), a checksum
//! file (`SHA256SUMS`) and an optional deterministic `.tar.gz`; and the verification of one.
//!
//! ```text
//! snapshot/
//!   snapshot.json      what this is: chain, genesis, game build, heights, counts, digests, versions
//!   SHA256SUMS         `sha256  size  path` for every other file (sorted)
//!   README.txt         the layout in a few lines (docs/EXPORT.md has the rest)
//!   manifest.json, cycles/, blocks/, records/, features.schema.json    (the export, docs/EXPORT.md)
//! ```
//!
//! `snapshot.json` pins the sha256 of `SHA256SUMS`, which pins every other file, so the one file is
//! enough to detect any change. [`verify`] checks the checksums, the structure (contiguous heights,
//! block ids chaining, counts), the game build, then replays everything (`walk`: every cycle from
//! power-on and every archived proof from its start, comparing every recorded state hash) and, where
//! the Mupen64Plus rig is available, the three-way check.

use crate::export::SCHEMA_VERSION;
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The version of `snapshot.json` itself.
pub const SNAPSHOT_VERSION: u32 = 1;
pub const SUMS: &str = "SHA256SUMS";
pub const SNAPSHOT_JSON: &str = "snapshot.json";

pub fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn sha256_file(p: &Path) -> Result<(String, u64)> {
    use std::io::Read;
    let mut f = std::fs::File::open(p).with_context(|| format!("opening {}", p.display()))?;
    let (mut h, mut buf, mut n) = (Sha256::new(), vec![0u8; 1 << 20], 0u64);
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((hex::encode(h.finalize()), n))
}

/// Every file under `dir` (relative, `/`-separated, sorted) except the two snapshot files.
pub fn list_files(dir: &Path) -> Result<Vec<String>> {
    fn rec(base: &Path, d: &Path, out: &mut Vec<String>) -> Result<()> {
        for e in std::fs::read_dir(d)? {
            let p = e?.path();
            if p.is_dir() {
                rec(base, &p, out)?;
            } else {
                out.push(p.strip_prefix(base)?.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"));
            }
        }
        Ok(())
    }
    let mut v = Vec::new();
    rec(dir, dir, &mut v)?;
    v.retain(|f| f != SUMS && f != SNAPSHOT_JSON);
    v.sort();
    Ok(v)
}

/// Writes `SHA256SUMS` and returns (its sha256, file count, total bytes).
pub fn write_sums(dir: &Path) -> Result<(String, usize, u64)> {
    let files = list_files(dir)?;
    let mut text = String::new();
    let mut total = 0;
    for f in &files {
        let (h, n) = sha256_file(&dir.join(f))?;
        text.push_str(&format!("{h}  {n}  {f}\n"));
        total += n;
    }
    std::fs::write(dir.join(SUMS), &text)?;
    Ok((sha256_hex(text.as_bytes()), files.len(), total))
}

/// What the snapshot says about the chain it was taken from.
#[derive(Clone, Debug, Default)]
pub struct ChainInfo {
    /// `mainnet`, `testnet`, `stagenet`, or `unknown` (a JSONL dump).
    pub network: String,
    pub node: Option<String>,
    pub node_height: Option<u64>,
    pub node_top_block: Option<String>,
    pub genesis_id: Option<String>,
}

pub fn iso_utc(unix: u64) -> String {
    let (days, rem) = (unix / 86400, unix % 86400);
    // Howard Hinnant's civil_from_days.
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y + (m <= 2) as i64, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

const README: &str = "SM64Coin chain dataset snapshot.

snapshot.json   what this is (chain, genesis, game build, heights, counts, versions, digests)
SHA256SUMS      sha256, size and path of every other file (`sm64coin-export verify-snapshot` checks it)
manifest.json   the export: cycles, blocks, records, totals
cycles/         one m64 per cycle: the effective inputs (pokes applied) from power-on
blocks/         per block: .json metadata, .pads (4 bytes/frame), start states, optional .features.bin
records/        v0.4 archived runs of each height (other miners' valid proofs), same start as the
                height's block: .json (miner key, payout address, counted for weight), .pads, ...
Layout and formats: docs/EXPORT.md.  Verify: sm64coin-export verify-snapshot <this dir or .tar.gz>
";

/// Writes the README, the checksums and `snapshot.json` for the export in `dir`.
pub fn finish(dir: &Path, chain: &ChainInfo, created_unix: u64) -> Result<Value> {
    std::fs::write(dir.join("README.txt"), README)?;
    let m: Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json"))?)?;
    let (sums_hash, files, bytes) = write_sums(dir)?;
    // The block ids in order: a digest that two snapshots of one chain share on their common prefix.
    let ids = block_ids(&m)?;
    let mut ids_hash = Sha256::new();
    ids.values().for_each(|i| ids_hash.update(i.as_bytes()));
    let snap = json!({
        "snapshot_version": SNAPSHOT_VERSION,
        "dataset": "sm64coin-chain-dataset",
        "schema_version": SCHEMA_VERSION,
        "created_utc": iso_utc(created_unix),
        "exporter": {"name": "sm64coin-export", "version": env!("CARGO_PKG_VERSION")},
        "chain": {
            "network": chain.network,
            "rules": m["rules"],
            "genesis_id": chain.genesis_id,
            "node": chain.node,
            "node_height": chain.node_height,
            "node_top_block": chain.node_top_block,
            "first_height": m["first_height"],
            "last_height": m["last_height"],
            "first_block_id": ids.values().next(),
            "last_block_id": ids.values().next_back(),
            "block_ids_sha256": hex::encode(ids_hash.finalize()),
        },
        "game": {"build": m["game_build"], "rom": m["rom"]},
        "counts": {
            "blocks": m["blocks"], "cycles": m["cycles"].as_array().map(|a| a.len()), "frames": m["frames"],
            "poked_frames": m["poked_frames"], "records": m["records"], "record_frames": m["record_frames"],
            "records_counted_for_weight": m["records_counted_for_weight"],
            "features": m["features"], "record_m64": m["record_m64"],
            "files": files, "bytes": bytes,
        },
        "sums_sha256": sums_hash,
    });
    std::fs::write(dir.join(SNAPSHOT_JSON), serde_json::to_string_pretty(&snap)? + "\n")?;
    Ok(snap)
}

fn block_ids(m: &Value) -> Result<BTreeMap<u64, String>> {
    let mut ids = BTreeMap::new();
    for c in m["cycles"].as_array().context("manifest.cycles")? {
        for b in c["blocks"].as_array().context("cycle blocks")? {
            ids.insert(b["height"].as_u64().context("height")?, b["id"].as_str().context("id")?.to_string());
        }
    }
    Ok(ids)
}

/// A deterministic `.tar.gz` of `dir` (sorted entries, mtime 0, owner 0, one top-level directory named
/// `top`): the same snapshot always gives the same bytes.
pub fn tar_gz(dir: &Path, out: &Path, top: &str) -> Result<()> {
    let gz = flate2::write::GzEncoder::new(std::fs::File::create(out)?, flate2::Compression::new(6));
    let mut tb = tar::Builder::new(gz);
    let mut files = list_files(dir)?;
    files.extend([SNAPSHOT_JSON.to_string(), SUMS.to_string()]);
    files.sort();
    for f in files {
        let data = std::fs::read(dir.join(&f))?;
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(0);
        h.set_uid(0);
        h.set_gid(0);
        h.set_cksum();
        tb.append_data(&mut h, format!("{top}/{f}"), data.as_slice())?;
    }
    tb.into_inner()?.finish()?;
    Ok(())
}

/// Unpacks a `.tar.gz` made by [`tar_gz`] into `into`; returns the snapshot directory inside it.
pub fn untar(archive: &Path, into: &Path) -> Result<PathBuf> {
    let gz = flate2::read::GzDecoder::new(std::fs::File::open(archive)?);
    let mut ar = tar::Archive::new(gz);
    std::fs::create_dir_all(into)?;
    // `unpack` refuses entries that escape `into`.
    ar.unpack(into)?;
    let mut dirs: Vec<_> = std::fs::read_dir(into)?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    ensure!(dirs.len() == 1, "the archive should hold one top-level directory");
    Ok(dirs.remove(0))
}

#[derive(Debug, Default)]
pub struct VerifyReport {
    pub problems: Vec<String>,
    pub files: usize,
    pub blocks: usize,
    pub records: usize,
    pub frames: u64,
    /// `ran: ...` / `skipped: why` / `DIFFERS: ...` for the three-way check, per movie.
    pub mupen: Vec<String>,
}

impl VerifyReport {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

#[derive(Clone, Debug, Default)]
pub struct VerifyOptions {
    /// Replay everything (off: files and structure only).
    pub replay: bool,
    /// Run the three-way check with stock Mupen64Plus in this work directory when the rig is available.
    pub mupen_work: Option<PathBuf>,
    /// How many archived proofs to check in Mupen64Plus, besides the cycles (needs `record_m64`).
    pub mupen_records: usize,
    pub mupen_tools: Option<PathBuf>,
    pub rom: Option<PathBuf>,
}

/// The structure and checksums only (no game needed).
pub fn check_files(dir: &Path, rep: &mut VerifyReport) -> Result<Value> {
    let snap: Value = serde_json::from_slice(&std::fs::read(dir.join(SNAPSHOT_JSON)).context("snapshot.json")?)?;
    let v = snap["snapshot_version"].as_u64().unwrap_or(0);
    ensure!(v >= 1 && v <= SNAPSHOT_VERSION as u64, "snapshot_version {v}: this tool reads up to {SNAPSHOT_VERSION}");
    let sv = snap["schema_version"].as_u64().unwrap_or(0);
    ensure!(sv >= 1 && sv <= SCHEMA_VERSION as u64, "schema_version {sv}: this tool reads up to {SCHEMA_VERSION}");
    let sums = std::fs::read_to_string(dir.join(SUMS)).context("SHA256SUMS")?;
    if sha256_hex(sums.as_bytes()) != snap["sums_sha256"].as_str().unwrap_or("") {
        rep.problems.push("SHA256SUMS does not match snapshot.json".into());
    }
    let mut listed = BTreeMap::new();
    for l in sums.lines() {
        let mut it = l.splitn(3, "  ");
        let (h, n, f) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
        listed.insert(f.to_string(), (h.to_string(), n.parse::<u64>().unwrap_or(u64::MAX)));
    }
    let on_disk = list_files(dir)?;
    for f in &on_disk {
        match listed.get(f) {
            None => rep.problems.push(format!("{f}: not in SHA256SUMS")),
            Some((h, n)) => {
                let (got, size) = sha256_file(&dir.join(f))?;
                if &got != h || size != *n {
                    rep.problems.push(format!("{f}: checksum or size differs"));
                }
            }
        }
    }
    for f in listed.keys() {
        if !on_disk.contains(f) {
            rep.problems.push(format!("{f}: listed but missing"));
        }
    }
    rep.files = on_disk.len();
    // Structure: heights contiguous, ids chain, counts agree with snapshot.json.
    let m: Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json"))?)?;
    let ids = block_ids(&m)?;
    let frames: u64 = m["cycles"].as_array().context("cycles")?.iter().map(|c| c["frames"].as_u64().unwrap_or(0)).sum();
    let (first, last) = (m["first_height"].as_u64().unwrap_or(0), m["last_height"].as_u64().unwrap_or(0));
    if ids.keys().copied().ne(first..=last) {
        rep.problems.push("block heights are not contiguous".into());
    }
    for (h, id) in &ids {
        let name = format!("h{:07}_{}", h, &id[..8]);
        match std::fs::read(dir.join(format!("blocks/{name}.json"))) {
            Err(_) => rep.problems.push(format!("block {h}: no metadata file")),
            Ok(b) => {
                let meta: Value = serde_json::from_slice(&b)?;
                if let Some(prev) = h.checked_sub(1).and_then(|p| ids.get(&p)) {
                    if meta["prev"].as_str() != Some(prev) {
                        rep.problems.push(format!("block {h}: prev does not match block {}", h - 1));
                    }
                }
            }
        }
    }
    let mut hasher = Sha256::new();
    ids.values().for_each(|i| hasher.update(i.as_bytes()));
    if hex::encode(hasher.finalize()) != snap["chain"]["block_ids_sha256"].as_str().unwrap_or("") {
        rep.problems.push("block_ids_sha256 differs".into());
    }
    let counts = &snap["counts"];
    let nrec = m["record_files"].as_array().map(|a| a.len() as u64).unwrap_or(0);
    for (k, got) in [("blocks", ids.len() as u64), ("frames", frames), ("records", m["records"].as_u64().unwrap_or(0))] {
        if counts[k].as_u64() != Some(got) {
            rep.problems.push(format!("counts.{k}: snapshot.json says {}, the files say {got}", counts[k]));
        }
    }
    if nrec != counts["records"].as_u64().unwrap_or(0) {
        rep.problems.push(format!("counts.records is {}, manifest lists {nrec} record files", counts["records"]));
    }
    rep.blocks = ids.len();
    Ok(snap)
}

/// Whether the Mupen64Plus rig can run here (the library, the HLE RSP plugin, the headers and gcc).
pub fn mupen_available() -> std::result::Result<(), String> {
    for p in ["/usr/lib/x86_64-linux-gnu/libmupen64plus.so.2", "/usr/lib/x86_64-linux-gnu/mupen64plus/mupen64plus-rsp-hle.so", "/usr/include/mupen64plus/m64p_common.h"] {
        if !Path::new(p).exists() {
            return Err(format!("{p} missing (libmupen64plus2, mupen64plus-rsp-hle, libmupen64plus-dev)"));
        }
    }
    if std::process::Command::new("gcc").arg("--version").output().is_err() {
        return Err("gcc missing".into());
    }
    Ok(())
}

fn find_tools(given: &Option<PathBuf>) -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = given.iter().cloned().collect();
    cands.extend(std::env::var("SM64COIN_MUPEN_TOOLS").ok().map(PathBuf::from));
    cands.push(PathBuf::from("pow/export/tools/mupen64plus"));
    cands.push(PathBuf::from("export/tools/mupen64plus"));
    if let Ok(exe) = std::env::current_exe() {
        for a in exe.ancestors().skip(1).take(5) {
            cands.push(a.join("export/tools/mupen64plus"));
            cands.push(a.join("pow/export/tools/mupen64plus"));
        }
    }
    cands.into_iter().find(|c| c.join("threeway_m64.sh").exists())
}

/// Verifies the snapshot in `dir`: files, structure, game build, replay, and the three-way check.
pub fn verify(dir: &Path, o: &VerifyOptions, log: &mut dyn FnMut(&str)) -> Result<VerifyReport> {
    let mut rep = VerifyReport::default();
    let snap = check_files(dir, &mut rep)?;
    log(&format!("{} files checked against SHA256SUMS; {} blocks, structure {}", rep.files, rep.blocks, if rep.ok() { "ok" } else { "BROKEN" }));
    if !o.replay {
        return Ok(rep);
    }
    let build = snap["game"]["build"].as_str().unwrap_or("");
    if build != sm64pow::GAME_BUILD {
        bail!("the snapshot was made with game build {build}, this binary is {}: state hashes are build-specific, so it cannot be replayed here", sm64pow::GAME_BUILD);
    }
    let r = crate::verify::verify(dir, None, log)?;
    rep.records = r.records;
    rep.frames = r.frames;
    rep.problems.extend(r.divergences);
    if let Some(work) = &o.mupen_work {
        three_way(dir, work, o, &mut rep, log)?;
    }
    Ok(rep)
}

fn three_way(dir: &Path, work: &Path, o: &VerifyOptions, rep: &mut VerifyReport, log: &mut dyn FnMut(&str)) -> Result<()> {
    if let Err(why) = mupen_available() {
        rep.mupen.push(format!("skipped: {why}"));
        log(&format!("three-way check skipped: {why}"));
        return Ok(());
    }
    let Some(tools) = find_tools(&o.mupen_tools) else {
        rep.mupen.push("skipped: tools/mupen64plus not found (--mupen-tools)".into());
        return Ok(());
    };
    let rom = o.rom.clone().or_else(|| sm64pow::default_rom_path().map(PathBuf::from)).ok_or_else(|| anyhow!("no ROM for the Mupen64Plus run"))?;
    let m: Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json"))?)?;
    let mut movies: Vec<String> = m["cycles"].as_array().unwrap().iter().map(|c| c["m64"].as_str().unwrap().to_string()).collect();
    if o.mupen_records > 0 {
        let mut n = 0;
        for r in m["record_files"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
            let meta: Value = serde_json::from_slice(&std::fs::read(dir.join(r.as_str().unwrap_or("")))?)?;
            if let Some(mv) = meta["files"]["m64"].as_str() {
                if n < o.mupen_records {
                    movies.push(mv.to_string());
                    n += 1;
                }
            }
        }
        if n == 0 {
            rep.mupen.push("skipped: archived proofs: the snapshot has no record m64s (export with --record-m64)".into());
        }
    }
    let exe = std::env::current_exe()?;
    for mv in movies {
        let out = std::process::Command::new("sh")
            .arg(tools.join("threeway_m64.sh"))
            .arg(dir.join(&mv))
            .arg(work)
            .env("SM64COIN_ROM", &rom)
            .env("SM64COIN_EXPORT", &exe)
            .output()?;
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        let last = text.lines().rev().find(|l| l.contains("polls compared")).unwrap_or("no result").to_string();
        log(&format!("three-way {mv}: {last}"));
        if out.status.success() {
            rep.mupen.push(format!("ran: {mv}: {last}"));
        } else {
            rep.mupen.push(format!("DIFFERS: {mv}: {last}"));
            rep.problems.push(format!("three-way (port vs Mupen64Plus) {mv}: {last}"));
        }
    }
    Ok(())
}
