//! Runs the rolling-horizon miner (rh.rs) offline on stand-in block seeds and
//! checks every proof with the consensus verifier.
use anyhow::Result;
use clap::Parser;
use sm64coin_miner::rh::{self, RhOptions};
use std::sync::atomic::AtomicBool;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "../baserom.us.z64")]
    rom: String,
    #[arg(long, default_value_t = 1)]
    seeds: u32,
    #[arg(long, default_value_t = 1)]
    first_seed: u32,
    #[arg(long, default_value_t = 1)]
    difficulty: u64,
    #[arg(long, default_value_t = 32)]
    pop: usize,
    #[arg(long, default_value_t = 600)]
    horizon: usize,
    #[arg(long, default_value_t = 4)]
    commit: usize,
    #[arg(long, default_value_t = 1)]
    gens: usize,
    #[arg(long, default_value_t = 4)]
    threads: usize,
    #[arg(long, default_value_t = 3)]
    window: usize,
}

fn main() -> Result<()> {
    let a = Args::parse();
    sm64pow::init(std::fs::read(&a.rom)?)?;
    let params = sm64pow::params(a.difficulty);
    println!("difficulty {}: {:?}", a.difficulty, params);
    let stop = AtomicBool::new(false);
    for n in a.first_seed..a.first_seed + a.seeds {
        let mut h = [0u8; 32];
        let mut x = (n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        for c in h.chunks_mut(8) {
            x ^= x >> 31;
            x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9).wrapping_add(0x94D0_49BB_1331_11EB);
            c.copy_from_slice(&x.to_le_bytes());
        }
        let seed = sm64pow::Seed(h);
        let o = RhOptions { pop: a.pop, horizon: a.horizon, commit: a.commit, gens: a.gens, threads: a.threads, window: a.window, seed: n as u64, ..Default::default() };
        match rh::mine(&seed, &params, &o, &stop, |m| println!("  [seed {n}] {m}"))? {
            Some(m) => {
                let blob = sm64pow::seal_proof(&seed, &params, &m.pads)?;
                let v = sm64pow::verify(&seed, a.difficulty, &blob)?;
                println!("seed {n}: SUCCESS {} frames in {:.0} s, simulated {} frames, proof {} bytes, verify: {:?}", m.pads.len(), m.seconds, m.sim_frames, blob.len(), v);
            }
            None => println!("seed {n}: failed"),
        }
    }
    Ok(())
}
