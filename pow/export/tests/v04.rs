//! v0.4 data tools on two real v0.4 testnet blocks (tests/fixtures/v04_testnet.jsonl: block 2 carries
//! two included proofs for height 1): the exporter's archived proofs, snapshots and their
//! verification, and the branching sampler. Needs the ROM (`SM64COIN_ROM`); skips without it.

use sm64coin_export::export::{Format, Options, States, export, fresh_game};
use sm64coin_export::m64::{self, Rom};
use sm64coin_export::sampler::{self, Kind};
use sm64coin_export::walk::{SegKind, walk};
use sm64coin_export::{snapshot, source, verify};
use std::path::PathBuf;
use std::sync::OnceLock;

fn rom() -> Option<Rom> {
    static R: OnceLock<Option<Rom>> = OnceLock::new();
    R.get_or_init(|| {
        let path = sm64pow::default_rom_path().or_else(|| Some("../../baserom.us.z64".into()).filter(|p| std::path::Path::new(p).exists()))?;
        let b = std::fs::read(path).ok()?;
        let info = Rom::from_z64(&b).ok()?;
        sm64pow::init(b).ok()?;
        Some(info)
    })
    .clone()
}

fn blocks() -> Vec<source::BlockRec> {
    source::read_jsonl(include_str!("fixtures/v04_testnet.jsonl")).unwrap()
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sm64coin-v04-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn opts(out: PathBuf, rom: Rom) -> Options {
    Options { out, rom, features: true, states: States::Portable, mupen64plus: false, clear_reserved_bits: false, format: Format::V04, record_m64: true, author: "test".into() }
}

/// One export shared by the tests (the replays are the slow part).
fn exported() -> Option<&'static PathBuf> {
    static D: OnceLock<Option<PathBuf>> = OnceLock::new();
    D.get_or_init(|| {
        let r = rom()?;
        let d = dir("shared");
        export(&blocks(), &opts(d.clone(), r), &mut |_| {}).unwrap();
        Some(d)
    })
    .as_ref()
}

fn json(p: PathBuf) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

#[test]
fn the_blocks_carry_their_included_proofs() {
    let b = blocks();
    assert_eq!(b.len(), 2);
    assert!(b[0].records.is_empty() && !b[1].records.is_empty());
    let recs = sm64pow::records::decode(&b[1].records).unwrap();
    assert_eq!(recs.len(), 2);
    assert!(b[1].proofs_root.is_some() && b[0].payout.is_some());
    // the JSONL round trip keeps them
    let again = source::read_jsonl(&b.iter().map(source::to_line).collect::<Vec<_>>().join("\n")).unwrap();
    assert_eq!(again[1].records, b[1].records);
    assert_eq!(again[1].payout, b[1].payout);
}

#[test]
fn archived_proofs_are_exported_as_segments_from_the_same_start() {
    let Some(d) = exported() else { return eprintln!("skipping: no ROM") };
    let m = json(d.join("manifest.json"));
    assert_eq!(m["schema_version"], 2);
    assert_eq!(m["rules"], "v0.4");
    assert_eq!(m["records"], 2);
    let files = m["record_files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    let block1 = json(std::fs::read_dir(d.join("blocks")).unwrap().map(|e| e.unwrap().path()).find(|p| p.to_string_lossy().contains("h0000001") && p.extension().unwrap() == "json").unwrap());
    assert_eq!(block1["proofs_archived"], 2);
    assert_eq!(json(std::fs::read_dir(d.join("blocks")).unwrap().map(|e| e.unwrap().path()).find(|p| p.to_string_lossy().contains("h0000002") && p.extension().unwrap() == "json").unwrap())["proofs_carried"], 2);
    let mut keys = std::collections::HashSet::new();
    for f in files {
        let r = json(d.join(f.as_str().unwrap()));
        assert_eq!(r["height"], 1);
        assert_eq!(r["included_in_height"], 2);
        assert_eq!(r["start_kind"], "power_on");
        assert_eq!(r["start_state_hash"], block1["start_state_hash"], "same start as the height's block");
        assert_eq!(r["start_same_as_block"], true);
        assert_ne!(r["end_state_hash"], block1["end_state_hash"]);
        assert_eq!(r["counted_for_weight"], true, "both are within the 60 s weight window");
        assert_eq!(r["miner_key"].as_str().unwrap().len(), 64);
        assert_eq!(r["payout"].as_str().unwrap().len(), 130);
        assert!(keys.insert(r["miner_key"].as_str().unwrap().to_string()), "one proof per key");
        assert_ne!(r["miner_key"], block1["miner_key"], "not the winner's key");
        // its pads file has the frames, the poked mask matches
        let pads = m64::frames_from_bytes(&std::fs::read(d.join(r["files"]["pads"].as_str().unwrap())).unwrap()).unwrap();
        assert_eq!(pads.len() as u64, r["frames"].as_u64().unwrap());
        let poked = sm64coin_export::export::unpack_mask(r["poked_mask_hex"].as_str().unwrap(), pads.len()).unwrap();
        assert_eq!(poked.iter().filter(|p| **p).count() as u64, r["poked_frames"].as_u64().unwrap());
        // the proof's movie: power-on start, no prefix, the same pads
        let (_, mp) = m64::read(&std::fs::read(d.join(r["files"]["m64"].as_str().unwrap())).unwrap()).unwrap();
        assert_eq!(mp, pads);
    }
}

#[test]
fn verify_replays_blocks_and_archived_proofs() {
    let Some(d) = exported() else { return eprintln!("skipping: no ROM") };
    let r = verify::verify(d, None, &mut |_| {}).unwrap();
    assert_eq!((r.cycles, r.blocks, r.records), (1, 2, 2));
    assert!(r.divergences.is_empty(), "{:?}", r.divergences);
    // a tampered record pad is caught
    let bad = dir("tamper");
    copy_dir(d, &bad);
    let rec = std::fs::read_dir(bad.join("records")).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().unwrap() == "pads").unwrap();
    let mut b = std::fs::read(&rec).unwrap();
    for i in 0..40 {
        b[4 * 3000 + i] ^= 0x80; // flip the button and stick bits of ten frames around frame 3000
    }
    std::fs::write(&rec, b).unwrap();
    let r = verify::verify(&bad, None, &mut |_| {}).unwrap();
    assert!(!r.divergences.is_empty(), "a changed proof must not replay to the recorded state");
}

fn copy_dir(a: &std::path::Path, b: &std::path::Path) {
    std::fs::create_dir_all(b).unwrap();
    for e in std::fs::read_dir(a).unwrap() {
        let e = e.unwrap();
        let t = b.join(e.file_name());
        if e.path().is_dir() { copy_dir(&e.path(), &t) } else { std::fs::copy(e.path(), t).unwrap(); }
    }
}

#[test]
fn snapshot_manifest_checksums_tarball_and_verification() {
    let Some(src) = exported() else { return eprintln!("skipping: no ROM") };
    let snap = dir("snap");
    copy_dir(src, &snap);
    let chain = snapshot::ChainInfo { network: "testnet".into(), genesis_id: Some(hex::encode(blocks()[0].prev)), ..Default::default() };
    let s = snapshot::finish(&snap, &chain, 1_795_680_000).unwrap();
    assert_eq!(s["snapshot_version"], 1);
    assert_eq!(s["created_utc"], "2026-11-26T08:00:00Z");
    assert_eq!(s["chain"]["genesis_id"], hex::encode(blocks()[0].prev));
    assert_eq!(s["chain"]["first_height"], 1);
    assert_eq!(s["chain"]["last_height"], 2);
    assert_eq!(s["counts"]["records"], 2);
    assert_eq!(s["game"]["build"], sm64pow::GAME_BUILD);
    // verification: files, structure, replay
    let vo = snapshot::VerifyOptions { replay: true, ..Default::default() };
    let r = snapshot::verify(&snap, &vo, &mut |_| {}).unwrap();
    assert!(r.ok(), "{:?}", r.problems);
    assert_eq!((r.blocks, r.records), (2, 2));
    // the archive is deterministic and unpacks to the same snapshot
    let (t1, t2) = (snap.with_extension("a.tgz"), snap.with_extension("b.tgz"));
    snapshot::tar_gz(&snap, &t1, "snap").unwrap();
    snapshot::tar_gz(&snap, &t2, "snap").unwrap();
    assert!(std::fs::read(&t1).unwrap() == std::fs::read(&t2).unwrap(), "the archive is deterministic");
    let un = dir("unpacked");
    let inner = snapshot::untar(&t1, &un).unwrap();
    let r = snapshot::verify(&inner, &snapshot::VerifyOptions::default(), &mut |_| {}).unwrap();
    assert!(r.ok(), "{:?}", r.problems);
    // any change is caught by the checksums
    let f = inner.join("records").read_dir().unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().unwrap() == "json").unwrap();
    let mut t = std::fs::read(&f).unwrap();
    t.push(b' ');
    std::fs::write(&f, t).unwrap();
    let r = snapshot::verify(&inner, &snapshot::VerifyOptions::default(), &mut |_| {}).unwrap();
    assert!(r.problems.iter().any(|p| p.contains("checksum")), "{:?}", r.problems);
    // and a missing file, an extra file
    std::fs::remove_file(inner.join("cycles/cycle_0000.m64")).unwrap();
    std::fs::write(inner.join("extra.txt"), "x").unwrap();
    let r = snapshot::verify(&inner, &snapshot::VerifyOptions::default(), &mut |_| {}).unwrap();
    assert!(r.problems.iter().any(|p| p.contains("missing")) && r.problems.iter().any(|p| p.contains("not in SHA256SUMS")), "{:?}", r.problems);
}

#[test]
fn iso_dates() {
    assert_eq!(snapshot::iso_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(snapshot::iso_utc(1_795_677_420), "2026-11-26T07:17:00Z");
    assert_eq!(snapshot::iso_utc(951_782_400), "2000-02-29T00:00:00Z");
}

// ---- the sampler ----

#[test]
fn stick_classes_are_the_input_analysis_tables() {
    let c = sampler::StickClasses::load();
    assert_eq!(c.count, 20129);
    // the dead zone is one class, (5,3) ~ (0,0); mirrored sticks differ; radius-64 sticks merge by angle bucket
    assert_eq!(c.class(5, 3), c.class(0, 0));
    assert_eq!(c.class(-7, 7), c.class(0, 0));
    assert_ne!(c.class(8, 0), c.class(0, 0));
    assert_eq!(c.class(127, 127), c.class(52, 52));
    assert_ne!(c.class(60, 0), c.class(-60, 0));
}

#[test]
fn pink_noise_is_correlated_and_hold_random_is_in_range() {
    let mut p = sampler::Pink::new(5);
    let f = [0u32; sampler::WORDS];
    let xs: Vec<f32> = (0..4096).map(|t| sampler::Policy::act(&mut p, &sampler::Obs { t, f: &f }).stick_x as f32).collect();
    let mean = xs.iter().sum::<f32>() / xs.len() as f32;
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / xs.len() as f32;
    let lag1 = xs.windows(2).map(|w| (w[0] - mean) * (w[1] - mean)).sum::<f32>() / xs.len() as f32 / var;
    assert!(lag1 > 0.6, "pink noise should be strongly correlated frame to frame, got {lag1}");
    let mut h = sampler::HoldRandom::new(9);
    let mut changes = 0;
    let mut last = None;
    for t in 0..2000 {
        let a = sampler::Policy::act(&mut h, &sampler::Obs { t, f: &f });
        assert_eq!(a.button & !(sampler::A | sampler::B | sampler::Z), 0);
        if last != Some((a.button, a.stick_x, a.stick_y)) {
            changes += 1;
        }
        last = Some((a.button, a.stick_x, a.stick_y));
    }
    assert!(changes < 400 && changes > 40, "holds of 4-24 frames: {changes} changes in 2000 frames");
}

#[test]
fn objective_policy_points_the_intended_yaw_at_the_target() {
    let mut f = [0u32; sampler::WORDS];
    for (i, v) in [100.0f32, 0.0, -50.0].iter().enumerate() {
        f[sampler::C_POS + i] = v.to_bits();
    }
    for cam in [0u32, 12345, 40000, 65000] {
        f[sampler::C_CAM_YAW] = cam;
        let target = [700.0f32, 0.0, 400.0];
        let mut pol = sampler::Objective::new(target, 0.0, 1);
        let p = sampler::Policy::act(&mut pol, &sampler::Obs { t: 0, f: &f });
        // the game's intendedYaw = atan2s(-stickY, stickX) + cameraYaw, atan2s(y, x) = angle of (x, y) from +y toward +x
        let yaw = (p.stick_x as f32).atan2(-(p.stick_y as f32)) * 65536.0 / std::f32::consts::TAU + cam as f32;
        let want = (target[0] - 100.0).atan2(target[2] + 50.0) * 65536.0 / std::f32::consts::TAU;
        let d = (yaw - want).rem_euclid(65536.0);
        let d = d.min(65536.0 - d);
        assert!(d < 700.0, "cam {cam}: yaw {yaw} vs {want} (diff {d} of 65536)");
    }
}

fn run_sampler(out: PathBuf, samplers: Vec<Kind>, seed: u64) -> sampler::Options {
    let o = sampler::Options { out, samplers, states: 12, branches: 3, horizon: 30, stride: 150, seed, ..Default::default() };
    sampler::sample(exported().unwrap(), &o, &mut |_| {}).unwrap();
    o
}

#[test]
fn sampler_is_deterministic_and_its_transitions_replay_exactly() {
    if exported().is_none() {
        return eprintln!("skipping: no ROM");
    }
    let (a, b) = (dir("s1"), dir("s2"));
    let oa = run_sampler(a.clone(), Kind::ALL.to_vec(), 7);
    run_sampler(b.clone(), Kind::ALL.to_vec(), 7);
    let stick = sampler::StickClasses::load();
    for k in Kind::ALL {
        let (da, db) = (a.join(k.name()), b.join(k.name()));
        for f in ["states.bin", "actions.bin", "trajectories.jsonl", "coverage.json"] {
            assert!(std::fs::read(da.join(f)).unwrap() == std::fs::read(db.join(f)).unwrap(), "{} {f}: same seed, same data", k.name());
        }
        let (states, actions) = sampler::read_arrays(&da, oa.horizon).unwrap();
        let t = actions.len() / oa.horizon;
        assert!(t > 0);
        assert_eq!(states.len(), t * (oa.horizon + 1) * sampler::WORDS);
        // every action's class is the table's, and its buttons are masked to the bits the game reads
        for &(btn, sx, sy, class, bclass) in &actions {
            assert_eq!(class, stick.class(sx, sy));
            assert_eq!(bclass, btn & sampler::VALID_BUTTONS);
        }
        // exact replay: from the recorded start (segment + offset), the recorded actions give the recorded states
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(da.join("trajectories.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), t);
        let (i, line) = (t - 1, lines.last().unwrap());
        let (h, name, off) = (line["segment"]["height"].as_u64().unwrap(), line["segment"]["name"].as_str().unwrap().to_string(), line["frame_offset"].as_u64().unwrap() as usize);
        let mut start = None;
        walk(exported().unwrap(), None, &mut |_| {}, &mut |seg| {
            if seg.height == h && seg.name == name && matches!(seg.kind, SegKind::Block | SegKind::Record) {
                let mut g = fresh_game()?;
                g.load_state(&seg.start)?;
                for p in &seg.pads[..off] {
                    g.step_game(*p)?;
                }
                start = Some(g.save_state());
            }
            Ok(())
        })
        .unwrap();
        let start = start.expect("the segment of the trajectory");
        assert_eq!(sm64coin_export::export::hash(&start), line["start_state_hash"].as_str().unwrap());
        let mut g = fresh_game().unwrap();
        g.load_state(&start).unwrap();
        let rec = sm64coin_export::features::record(&g).unwrap();
        let row = |s: usize| &states[(i * (oa.horizon + 1) + s) * sampler::WORDS..(i * (oa.horizon + 1) + s + 1) * sampler::WORDS];
        let words = |r: &[u8]| r.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect::<Vec<_>>();
        assert_eq!(words(&rec), row(0));
        for step in 0..oa.horizon {
            let (btn, sx, sy, ..) = actions[i * oa.horizon + step];
            g.step_game(sm64_binds::GamePad::new(btn, sx, sy)).unwrap();
            assert_eq!(words(&sm64coin_export::features::record(&g).unwrap()), row(step + 1), "{} frame {step}", k.name());
        }
    }
    // a different seed gives different data
    let c = dir("s3");
    run_sampler(c.clone(), vec![Kind::Random], 8);
    assert!(std::fs::read(a.join("random/actions.bin")).unwrap() != std::fs::read(c.join("random/actions.bin")).unwrap());
}

#[test]
fn samplers_differ_in_the_measured_ways() {
    if exported().is_none() {
        return eprintln!("skipping: no ROM");
    }
    let out = dir("cmp");
    let o = run_sampler(out.clone(), Kind::ALL.to_vec(), 3);
    let man = json(out.join("manifest.json"));
    assert_eq!(man["format"], sampler::FORMAT);
    assert_eq!(man["settings"]["horizon"], o.horizon);
    let cov = |k: &str| man["coverage"][k].clone();
    for k in Kind::ALL {
        let c = cov(k.name());
        assert!(c["trajectories"].as_u64().unwrap() > 0);
        assert_eq!(c["frames"].as_u64().unwrap(), c["trajectories"].as_u64().unwrap() * o.horizon as u64);
        assert!(c["cells_visited"].as_u64().unwrap() > 0);
    }
    // the pink sampler uses a wider range of sticks than random holds (a measured difference, not an assumed one)
    assert!(cov("random")["stick_classes_used"].as_u64().unwrap() > cov("uniform")["stick_classes_used"].as_u64().unwrap());
    // the jitter sampler's branch 0 is the chain's own continuation: its first trajectory replays the segment's pads
    let first: serde_json::Value = serde_json::from_str(std::fs::read_to_string(out.join("jitter/trajectories.jsonl")).unwrap().lines().next().unwrap()).unwrap();
    assert_eq!(first["branch"], 0);
    assert_eq!(first["jitter_rate"], 0.0);
    // the objective sampler records its target and the distances
    let obj = cov("objective")["objective"].clone();
    assert!(obj["trajectories"].as_u64().unwrap() > 0 && obj["mean_distance_min"].as_f64().unwrap() <= obj["mean_distance_start"].as_f64().unwrap());
}
