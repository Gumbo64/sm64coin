//! Writes a parent end state (gzip of the SaveState bytes) for the settle tests: the end of
//! sm64pow's power_on_star fixture (BoB star 6, the chomp star), as a miner gets a parent's state.
//! Usage: mkparent <out.state.gz>
use flate2::{Compression, write::GzEncoder};
use sm64pow::chain::{Parent, played_block};
use sm64pow::Seed;
use std::io::Write;

fn main() -> anyhow::Result<()> {
    sm64pow::init_default()?;
    let j: serde_json::Value = serde_json::from_str(include_str!("../../sm64pow/tests/fixtures/power_on_star.json"))?;
    let seed = Seed::from_hex(j["seed_hash"].as_str().unwrap()).unwrap();
    let hex = j["proof_hex"].as_str().unwrap();
    let blob: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap()).collect();
    let played = played_block(&[11; 32], Parent::Genesis, &seed, &blob)?.map_err(|v| anyhow::anyhow!("{v:?}"))?;
    let mut g = sm64pow::generator()?.create_game()?;
    for pad in &played {
        g.step_game(*pad)?;
    }
    println!("{} frames, progress {:?}", played.len(), sm64pow::game::read_progress(&g)?.stars);
    let mut e = GzEncoder::new(Vec::new(), Compression::best());
    e.write_all(g.save_state().as_bytes())?;
    std::fs::write(std::env::args().nth(1).unwrap(), e.finish()?)?;
    Ok(())
}
