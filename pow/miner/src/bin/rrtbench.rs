//! Runs the RRT miner (rrt/mod.rs) offline on stand-in block seeds and checks
//! every proof with the consensus verifier. Block 1 starts from power-on;
//! `--chain N` mines N more blocks, each from the previous block's end state.
//! `--fixture` starts from the end state of a fixture's block 1.
use anyhow::Result;
use clap::Parser;
use sm64coin_miner::rh::Attempt;
use sm64coin_miner::rrt::{self, RrtOptions};
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
    #[arg(long, default_value_t = 4)]
    threads: usize,
    /// Emulated frames a castle tree / a course tree may use.
    #[arg(long, default_value_t = 16_000_000)]
    hub_frames: u64,
    #[arg(long, default_value_t = 60_000_000)]
    course_frames: u64,
    #[arg(long, default_value_t = 900.0)]
    step_seconds: f64,
    #[arg(long)]
    fixture: Option<String>,
    /// Further blocks after block 1.
    #[arg(long, default_value_t = 0)]
    chain: u32,
    #[arg(long)]
    keep: bool,
    /// After each found run, seal it plain, with the exact canonicaliser (sm64pow::exact) and with the
    /// replay-guided normaliser, and print sizes and seconds (the proof stays the plain one).
    #[arg(long)]
    compare_normalisers: bool,
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

fn main() -> Result<()> {
    let a = Args::parse();
    sm64pow::init(std::fs::read(&a.rom)?)?;
    let params = sm64pow::params(a.difficulty);
    println!("difficulty {} (limit {} frames at elapsed 0)", a.difficulty, sm64pow::rules().max_frames(a.difficulty, 0));
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
            let o = RrtOptions {
                threads: a.threads,
                hub_frames: a.hub_frames,
                course_frames: a.course_frames,
                step_seconds: a.step_seconds,
                seed: n as u64,
                rom: Some(a.rom.clone()),
                keep: a.keep,
                ..Default::default()
            };
            let label = format!("seed {n} block {}", block + 1);
            let (start, state) = match parent {
                None => (Start::PowerOn, None),
                Some(id) => (Start::Parent, chain::cached(&id).map(|e| e.game.clone())),
            };
            let before = match &state {
                Some(s) => {
                    let mut g = sm64pow::generator()?.create_game()?;
                    g.load_state(s)?;
                    sm64pow::game::read_progress(&g)?
                }
                None => Default::default(),
            };
            let t0 = std::time::Instant::now();
            match rrt::mine(start, state.as_ref(), &seed, &params, &o, &stop, |m| println!("  [{label}] {m}"))? {
                Attempt::Found(m) => {
                    let t_plain = std::time::Instant::now();
                    let blob = sm64pow::seal_proof_at(start, state.as_ref(), &seed, &params, &m.pads)?;
                    let parent_kind = match parent {
                        None => chain::Parent::Genesis,
                        Some(pid) => chain::Parent::Block { id: pid, gap_seconds: 0 },
                    };
                    // v0.4 proofs are committed blobs: the node's check is verify_any.
                    let verdict = |b: &[u8]| -> Result<chain::BlockVerdict> {
                        let mut id = sm64pow::proof_id(b);
                        id[0] ^= 0xff;
                        Ok(match chain::verify_any(&id, parent_kind, &seed, a.difficulty, b)? {
                            chain::CommittedVerdict::Run(v) => v,
                            v => chain::BlockVerdict::Invalid(format!("{v:?}")),
                        })
                    };
                    if a.compare_normalisers {
                        let plain_secs = t_plain.elapsed().as_secs_f64();
                        use sm64pow::normalise::{Method, StickPick, seal_proof_exact_report, seal_proof_normalised_report};
                        let t1 = std::time::Instant::now();
                        let ex = seal_proof_exact_report(start, state.as_ref(), &seed, &params, &m.pads, Method::Exact, StickPick::Model)?;
                        let ex_secs = t1.elapsed().as_secs_f64();
                        let t2 = std::time::Instant::now();
                        let rp = seal_proof_normalised_report(start, state.as_ref(), &seed, &params, &m.pads)?;
                        println!(
                            "{label}: NORMALISERS {} frames: plain {} B ({plain_secs:.2} s); exact {} B ({:.1} %, {ex_secs:.2} s, exact canonicaliser {:.2} s, {} sticks changed, {} bits cleared); replay-guided {} B ({:.1} %, {:.1} s)",
                            m.pads.len(), blob.len(), ex.0.len(), 100.0 * ex.0.len() as f64 / blob.len() as f64, ex.1.exact_secs, ex.1.stats.sticks_changed, ex.1.stats.button_bits_cleared,
                            rp.0.len(), 100.0 * rp.0.len() as f64 / blob.len() as f64, t2.elapsed().as_secs_f64()
                        );
                        let (vp, ve, vr) = (verdict(&blob)?, verdict(&ex.0)?, verdict(&rp.0)?);
                        println!("{label}: NORMALISERS verifier (verify_any): plain {vp:?}; exact same verdict: {}; replay-guided same verdict: {}", ve == vp, vr == vp);
                    }
                    let id = sm64pow::proof_id(&blob);
                    let v = verdict(&blob)?;
                    let ev = match &v {
                        chain::BlockVerdict::Valid { progress, .. } => sm64pow::game::event(&before, progress).map(|e| e.name()),
                        _ => None,
                    };
                    println!(
                        "{label}: SUCCESS event {:?} {} frames, wall {:.0} s, simulated {} frames, proof {} bytes, verify: {:?}",
                        ev, m.pads.len(), t0.elapsed().as_secs_f64(), m.sim_frames, blob.len(), v
                    );
                    if !matches!(v, chain::BlockVerdict::Valid { .. }) {
                        println!("{label}: INVALID PROOF");
                        break;
                    }
                    parent = Some(id);
                }
                Attempt::Gave(why) => {
                    println!("{label}: no block ({why}) after {:.0} s", t0.elapsed().as_secs_f64());
                    break;
                }
                Attempt::NoRoute(why) => {
                    println!("{label}: no route ({why})");
                    break;
                }
            }
        }
    }
    Ok(())
}
