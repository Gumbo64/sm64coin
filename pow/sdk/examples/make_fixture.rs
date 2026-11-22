//! Generates a winning run for a practice seed with the built-in miner's
//! rolling-horizon controller (power-on start, so it takes a few minutes). Used
//! to make a real, valid proof that is long enough to contain many pokes:
//!
//!   RAYON_NUM_THREADS=4 cargo run --release -p sm64coin-sdk --example make_fixture -- [seed]
//!
//! Writes pow/sdk/fixtures/win_seed<N>_d1.json (seed_hash, proof_hex, frames).
use sm64coin_miner::rh;
use sm64coin_sdk::*;
use std::sync::atomic::AtomicBool;

fn main() -> anyhow::Result<()> {
    let seed: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    init_rom("")?;
    let task = Task::practice(seed, 1);
    let o = rh::RhOptions { seed, ..Default::default() };
    let m = match rh::mine(proof::Start::PowerOn, None, &task.seed, &task.params, &o, &AtomicBool::new(false), |m| println!("  {m}"))? {
        rh::Attempt::Found(m) => m,
        _ => anyhow::bail!("no proof found"),
    };
    let blob = sm64pow::seal_proof_at(proof::Start::PowerOn, None, &task.seed, &task.params, &m.pads)?;
    assert_eq!(task.check(&blob)?, Verdict::Valid { frames: m.pads.len() as u32 });
    let seed_hex: String = task.seed.0.iter().map(|b| format!("{b:02x}")).collect();
    let j = serde_json::json!({"practice_seed": seed, "seed_hash": seed_hex, "difficulty": 1, "frames": m.pads.len(), "proof_hex": hex::encode(&blob)});
    std::fs::create_dir_all("pow/sdk/fixtures")?;
    let path = format!("pow/sdk/fixtures/win_seed{seed}_d1.json");
    std::fs::write(&path, serde_json::to_string_pretty(&j)?)?;
    println!("wrote {path}: {} frames", m.pads.len());
    Ok(())
}
