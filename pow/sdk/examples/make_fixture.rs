//! Generates tests/fixtures: a winning run for a practice seed, found with the
//! built-in miner's beam search (so it can take a while). Used by the Rust and
//! Python tests as a real, valid proof that is long enough to contain many pokes.
//!
//!   RAYON_NUM_THREADS=4 cargo run --release -p sm64coin-sdk --example make_fixture -- [seed] [route.m64]
use sm64coin_miner::{beam, tracker};
use sm64coin_sdk::*;
use std::sync::atomic::AtomicBool;

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let seed: u64 = a.first().and_then(|s| s.parse().ok()).unwrap_or(1);
    let route = a.get(1).cloned().unwrap_or("pow/miner/routes/block1-human.m64".into());
    init_rom("")?;
    let task = Task::practice(seed, 1);
    let reference = tracker::Reference::record(&load_m64(&route)?)?;
    let cfg = beam::BeamConfig { seed: 1, ..Default::default() };
    let m = beam::mine(&task.seed, &task.params, &reference, &cfg, &AtomicBool::new(false), |m| println!("  {m}"))?
        .ok_or_else(|| anyhow::anyhow!("no proof found"))?;
    let mut raw = Vec::new();
    for p in &m.pads {
        raw.extend(proof::pad_bytes(p));
    }
    let seed_hex: String = task.seed.0.iter().map(|b| format!("{b:02x}")).collect();
    let j = serde_json::json!({"practice_seed": seed, "seed_hash": seed_hex, "difficulty": 1, "frames": m.pads.len(), "pads_raw_hex": hex::encode(&raw)});
    let path = format!("pow/sdk/fixtures/win_seed{seed}_d1.json");
    std::fs::write(&path, serde_json::to_string_pretty(&j)?)?;
    println!("wrote {path}: {} frames", m.pads.len());
    Ok(())
}
