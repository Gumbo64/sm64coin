//! The fixed cost of one replay (DoS: a claim that fails after a few frames still pays it):
//! `replay_overhead <rom>` caches block 1 of tests/fixtures/mainnet_v02.jsonl, then verifies
//! junk claims of 300 and 3000 frames on it (from the parent's end state) and from power-on,
//! each with a fresh seed (no cache hit), and prints the time per replay.
use sm64pow::Seed;
use sm64pow::chain::{Parent, verify_block};
use sm64pow::proof::{Proof, Start, encode_proof};
use sm64pow::sm64_binds::GamePad;
use std::time::Instant;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    sm64pow::init(std::fs::read(&a[1])?)?;
    let l = include_str!("../tests/fixtures/mainnet_v02.jsonl").lines().next().unwrap();
    let j: serde_json::Value = serde_json::from_str(l)?;
    let id: [u8; 32] = hex(j["id"].as_str().unwrap()).try_into().unwrap();
    let seed = Seed::from_hex(j["seed"].as_str().unwrap())?;
    let t = Instant::now();
    let v = verify_block(&id, Parent::Genesis, &seed, 1, &hex(j["proof"].as_str().unwrap()))?;
    println!("block 1: {v:?} in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    for (start, frames) in [(Start::Parent, 300u32), (Start::Parent, 3000), (Start::PowerOn, 300)] {
        let n = 10;
        let t = Instant::now();
        for k in 0..n {
            let mut inputs = vec![GamePad::default(); frames as usize];
            inputs.last_mut().unwrap().button = k as u16 + 1;
            let blob = encode_proof(&Proof { frames, start, inputs });
            let mut s = [0u8; 32];
            s[0] = k as u8;
            s[1] = frames as u8;
            s[2] = start as u8;
            let parent = Parent::Block { id, gap_seconds: 100_000 };
            let mut jid = [7u8; 32];
            jid[..3].copy_from_slice(&s[..3]);
            let _ = verify_block(&jid, parent, &Seed(s), 1, &blob)?;
        }
        println!("{start:?} start, {frames} junk frames: {:.1} ms per replay", t.elapsed().as_secs_f64() * 1e3 / n as f64);
    }
    // Where the fixed cost goes: a fresh emulator, and loading an 8 MB end state into it.
    let n = 10;
    let t = Instant::now();
    let mut g = sm64pow::generator()?.create_game()?;
    for _ in 1..n {
        g = sm64pow::generator()?.create_game()?;
    }
    println!("create_game: {:.2} ms", t.elapsed().as_secs_f64() * 1e3 / n as f64);
    for _ in 0..2000 {
        g.step_game(GamePad::default())?;
    }
    let st = g.save_state();
    let t = Instant::now();
    for _ in 0..n {
        g.load_state(&st)?;
    }
    println!("load_state: {:.2} ms", t.elapsed().as_secs_f64() * 1e3 / n as f64);
    let t = Instant::now();
    for _ in 0..n {
        let _ = sm64pow::game::read_region_bytes(&g)?;
    }
    println!("read_region_bytes: {:.3} ms", t.elapsed().as_secs_f64() * 1e3 / n as f64);
    Ok(())
}
