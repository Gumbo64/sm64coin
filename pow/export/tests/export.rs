//! End to end on real v0.2 mainnet blocks (sm64pow's fixtures). Needs the ROM
//! (`SM64COIN_ROM`); skips without it, like sm64pow's replay tests.

use sm64coin_export::export::{Options, States, export, fresh_game, play_plain};
use sm64coin_export::m64::{self, Rom};
use sm64coin_export::{features, source, verify};
use std::path::PathBuf;

fn rom() -> Option<Rom> {
    let path = sm64pow::default_rom_path().or_else(|| Some("../../baserom.us.z64".into()).filter(|p| std::path::Path::new(p).exists()))?;
    let b = std::fs::read(path).ok()?;
    let info = Rom::from_z64(&b).ok()?;
    sm64pow::init(b).ok()?;
    Some(info)
}

fn blocks(n: usize) -> Vec<source::BlockRec> {
    let mut v = source::read_jsonl(include_str!("../../sm64pow/tests/fixtures/mainnet_v02.jsonl")).unwrap();
    v.truncate(n);
    v
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sm64coin-export-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn opts(out: PathBuf, rom: Rom) -> Options {
    Options { out, rom, features: true, states: States::Portable, mupen64plus: true, clear_reserved_bits: false, author: "test".into() }
}

#[test]
fn export_and_verify_five_blocks() {
    let Some(rom_info) = rom() else { return eprintln!("skipping: no ROM") };
    let out = dir("five");
    let s = export(&blocks(5), &opts(out.clone(), rom_info), &mut |_| {}).unwrap();
    // The plain replay of the effective pads (no perturbation) ends in the chain replay's exact RAM image.
    assert_eq!(s.plain_replay_mismatches, Vec::<u64>::new());
    assert_eq!(s.portable_mismatches, Vec::<u64>::new());
    assert_eq!((s.blocks, s.cycles, s.frames), (5, 1, 18390));
    assert!(s.poked > 1000, "pokes were applied");

    let r = verify::verify(&out, None, &mut |_| {}).unwrap();
    assert_eq!((r.cycles, r.blocks, r.divergences.len()), (1, 5, 0));

    // The cycle's m64: valid header, power-on start, all frames; the Mupen64Plus variant has one more (zero) poll.
    let (h, pads) = m64::read(&std::fs::read(out.join("cycles/cycle_0000.m64")).unwrap()).unwrap();
    assert_eq!((h.samples, h.start_type, h.controllers), (18390, 2, 1));
    assert_eq!(h.rom, Rom::sm64_us());
    let (_, mp) = m64::read(&std::fs::read(out.join("cycles/cycle_0000.mupen64plus.m64")).unwrap()).unwrap();
    assert_eq!((mp.len(), mp[0], &mp[1..]), (18391, Default::default(), &pads[..]));

    // A block's files agree with each other and with the game.
    let m: serde_json::Value = serde_json::from_slice(&std::fs::read(out.join("blocks/h0000002_41120ad5.json")).unwrap()).unwrap();
    let frames = m["frames"].as_u64().unwrap() as usize;
    assert_eq!((frames, m["start_kind"].as_str(), m["event"].as_str()), (2789, Some("parent"), Some("c02s3")));
    assert_eq!(m["progress_before"]["points"], 1);
    assert_eq!(m["progress_after"]["points"], 2);
    let mask = sm64coin_export::export::unpack_mask(m["poked_mask_hex"].as_str().unwrap(), frames).unwrap();
    assert_eq!(mask.iter().filter(|p| **p).count() as u64, m["poked_frames"].as_u64().unwrap());
    // 5 bursts of 2 per 100 game ticks: about 10% of the frames.
    let share = m["poked_frames"].as_f64().unwrap() / frames as f64;
    assert!((0.05..0.15).contains(&share), "{share}");
    let f = std::fs::read(out.join(m["files"]["features"].as_str().unwrap())).unwrap();
    assert_eq!(f.len(), (frames + 1) * features::RECORD_BYTES);
    let col = |rec: usize, c: usize| f32::from_le_bytes(f[rec * features::RECORD_BYTES + c * 4..][..4].try_into().unwrap());
    let px = features::COLUMNS.iter().position(|c| c.0 == "pos_x").unwrap();
    let end = m["end_pos"].as_array().unwrap();
    assert_eq!(col(frames, px), end[0].as_f64().unwrap() as f32);
    assert_eq!(m["files"]["start_portable_exact"], true);

    // Start states: the block's start (the parent's end) hashes as the parent's end_state_hash.
    let p: serde_json::Value = serde_json::from_slice(&std::fs::read(out.join("blocks/h0000001_b18f65f5.json")).unwrap()).unwrap();
    assert_eq!(m["start_state_hash"], p["end_state_hash"]);
    let _ = std::fs::remove_dir_all(out);
}

#[test]
fn a_tampered_m64_is_caught() {
    let Some(rom_info) = rom() else { return eprintln!("skipping: no ROM") };
    let out = dir("tamper");
    let mut o = opts(out.clone(), rom_info);
    (o.features, o.states, o.mupen64plus) = (false, States::None, false);
    export(&blocks(2), &o, &mut |_| {}).unwrap();
    let f = out.join("cycles/cycle_0000.m64");
    let mut b = std::fs::read(&f).unwrap();
    let at = m64::HEADER_LEN + 4 * 3000;
    b[at + 2] ^= 0x40; // a stick bit, mid first block
    std::fs::write(&f, b).unwrap();
    let r = verify::verify(&out, None, &mut |_| {}).unwrap();
    assert!(!r.divergences.is_empty());
    let _ = std::fs::remove_dir_all(out);
}

#[test]
fn camera_columns_match_the_games_own_state() {
    let Some(_) = rom() else { return eprintln!("skipping: no ROM") };
    let mut g = fresh_game().unwrap();
    // Walk the first block's effective pads until the camera has moved, then compare.
    let b = &blocks(1)[0];
    let played = sm64pow::chain::played_wanted_block(&b.id, sm64pow::chain::Parent::Genesis, &sm64pow::Seed(b.seed), &b.proof).unwrap().unwrap();
    let mut feats = Vec::new();
    play_plain(&mut g, &played.pads[..2000], Some(&mut feats)).unwrap();
    let gs = g.get_game_state().unwrap();
    let last = &feats[2000 * features::RECORD_BYTES..];
    let idx = |n: &str| features::COLUMNS.iter().position(|c| c.0 == n).unwrap();
    let f = |n: &str| f32::from_le_bytes(last[idx(n) * 4..][..4].try_into().unwrap());
    assert_eq!([f("cam_pos_x"), f("cam_pos_y"), f("cam_pos_z")], gs.lakitu_pos);
    assert_eq!([f("pos_x"), f("pos_y"), f("pos_z")], gs.pos);
    assert_eq!([f("vel_x"), f("vel_y"), f("vel_z")], gs.vel);
}

#[test]
fn a_dump_must_start_at_a_power_on_block() {
    let Some(rom_info) = rom() else { return eprintln!("skipping: no ROM") };
    let out = dir("mid");
    let b = blocks(3);
    // Block 2 continues from block 1's end state, which a dump starting at 2 doesn't have.
    let e = export(&b[1..], &opts(out.clone(), rom_info), &mut |_| {}).err().expect("must refuse").to_string();
    assert!(e.contains("start the export at a block that starts from power-on"), "{e}");
    let _ = std::fs::remove_dir_all(out);
}

/// All 34 v0.2 blocks: the portable start states of blocks 11 and 32 do not continue exactly under
/// sm64-binds 2.1.1's translator (an object's raw words were relocated as pointers: a small penguin's
/// state in block 11, see docs/DIVERGENCE.md); sm64-binds >= 2.3.1 (commit 5b7bfd2) fixes it. The test
/// accepts both, and fails on any other block, so a translator regression shows up.
/// Slow (~20 s), so run with `--ignored`.
#[test]
#[ignore]
fn portable_states_of_all_fixture_blocks() {
    let Some(rom_info) = rom() else { return eprintln!("skipping: no ROM") };
    let out = dir("all");
    let s = export(&blocks(1000), &opts(out, rom_info), &mut |_| {}).unwrap();
    assert_eq!(s.plain_replay_mismatches, Vec::<u64>::new());
    let ok = s.portable_mismatches.is_empty() || s.portable_mismatches == vec![11, 32];
    assert!(ok, "portable mismatches {:?}", s.portable_mismatches);
}
