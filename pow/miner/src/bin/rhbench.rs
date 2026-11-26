//! Runs the rolling-horizon miner (rh.rs) offline on stand-in block seeds and
//! checks every proof with the consensus verifier. Block 1 starts from
//! power-on; with `--chain N`, N more blocks follow, each from the previous
//! block's end state (the verifier's chain cache), with their own seeds.
use anyhow::Result;
use clap::Parser;
use sm64coin_miner::rh::{self, RhOptions};
use sm64pow::chain;
use sm64pow::proof::Start;
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
    /// Skip block 1: start from the end state of this fixture's block 1 (a JSON file with seed_hash and proof_hex, e.g. pow/sm64pow/tests/fixtures/power_on_star.json), then mine `--chain` more blocks.
    #[arg(long)]
    fixture: Option<String>,
    /// Further blocks mined after block 1, each from the previous end state.
    #[arg(long, default_value_t = 0)]
    chain: u32,
}

fn main() -> Result<()> {
    let a = Args::parse();
    sm64pow::init(std::fs::read(&a.rom)?)?;
    let params = sm64pow::params(a.difficulty);
    println!("difficulty {} (limit {} frames at elapsed 0): {:?}", a.difficulty, sm64pow::rules().max_frames(a.difficulty, 0), params);
    let stop = AtomicBool::new(false);
    for n in a.first_seed..a.first_seed + a.seeds {
        let mut parent: Option<[u8; 32]> = None;
        let mut first = 0;
        if let Some(f) = &a.fixture {
            let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(f)?)?;
            let seed = sm64pow::Seed::from_hex(j["seed_hash"].as_str().unwrap())?;
            let blob = hex::decode(j["proof_hex"].as_str().unwrap())?;
            let id = sm64pow::proof_id(&blob);
            let v = chain::verify_block(&id, chain::Parent::Genesis, &seed, a.difficulty, &blob)?;
            println!("fixture block 1: {v:?}");
            parent = Some(id);
            first = 1;
        }
        for block in first..=a.chain.max(first) {
            let seed = stand_in_seed(n as u64 * 1000 + block as u64);
            let o = RhOptions { pop: a.pop, horizon: a.horizon, commit: a.commit, gens: a.gens, threads: a.threads, window: a.window, seed: n as u64, ..Default::default() };
            let label = format!("seed {n} block {}", block + 1);
            let (start, state) = match parent {
                None => (Start::PowerOn, None),
                Some(id) => (Start::Parent, chain::cached(&id).map(|e| e.game.clone())),
            };
            let t0 = std::time::Instant::now();
            match rh::mine(start, state.as_ref(), &seed, &params, &o, &stop, |m| println!("  [{label}] {m}"))? {
                rh::Attempt::Found(m) => {
                    let blob = sm64pow::seal_proof_at(start, state.as_ref(), &seed, &params, &m.pads)?;
                    let id = sm64pow::proof_id(&blob);
                    let p = match parent {
                        None => chain::Parent::Genesis,
                        Some(pid) => chain::Parent::Block { id: pid, gap_seconds: 0 },
                    };
                    // v0.4: the sealed proof is a committed blob (header with the input hash), verified as such.
                    let v = match chain::verify_committed_block(&id, p, &seed, a.difficulty, &blob)? {
                        chain::CommittedVerdict::Run(v) => v,
                        chain::CommittedVerdict::BadEncoding(e) => chain::BlockVerdict::Invalid(format!("bad encoding: {e}")),
                    };
                    println!("{label}: SUCCESS {} frames in {:.0} s (wall {:.0} s), simulated {} frames, proof {} bytes, verify: {:?}", m.pads.len(), m.seconds, t0.elapsed().as_secs_f64(), m.sim_frames, blob.len(), v);
                    if !matches!(v, chain::BlockVerdict::Valid { .. }) {
                        break;
                    }
                    parent = Some(id);
                }
                rh::Attempt::Gave(why) => {
                    println!("{label}: failed ({why}) after {:.0} s", t0.elapsed().as_secs_f64());
                    break;
                }
                rh::Attempt::NoRoute(why) => {
                    println!("{label}: no route ({why})");
                    break;
                }
            }
        }
    }
    Ok(())
}

fn stand_in_seed(n: u64) -> sm64pow::Seed {
    let mut h = [0u8; 32];
    let mut x = n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for c in h.chunks_mut(8) {
        x ^= x >> 31;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9).wrapping_add(0x94D0_49BB_1331_11EB);
        c.copy_from_slice(&x.to_le_bytes());
    }
    sm64pow::Seed(h)
}
