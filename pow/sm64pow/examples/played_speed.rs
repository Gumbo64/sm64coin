//! Time of the node's played-pads replay (chain::played_block, what get_sm64_cycle runs per
//! block): `played_speed <rom> <blocks json>` (id, prev, genesis_parent, gap, seed, proof hex).
use std::time::Instant;
fn h32(s: &str) -> [u8; 32] {
    let mut o = [0u8; 32];
    for i in 0..32 { o[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap(); }
    o
}
fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    sm64pow::init(std::fs::read(&a[1])?)?;
    let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&a[2])?)?;
    let (mut frames, t0) = (0usize, Instant::now());
    for b in j.as_array().unwrap() {
        let s = |k: &str| b[k].as_str().unwrap().to_string();
        let proof: Vec<u8> = (0..s("proof").len() / 2).map(|i| u8::from_str_radix(&s("proof")[2 * i..2 * i + 2], 16).unwrap()).collect();
        let parent = if b["genesis_parent"].as_bool().unwrap() { sm64pow::chain::Parent::Genesis } else { sm64pow::chain::Parent::Block { id: h32(&s("prev")), gap_seconds: b["gap"].as_u64().unwrap() } };
        let t = Instant::now();
        let r = sm64pow::chain::played_block_opts(&h32(&s("id")), parent, &sm64pow::Seed(h32(&s("seed"))), &proof, true)?;
        let n = r.as_ref().map(|p| p.len()).unwrap_or(0);
        frames += n;
        println!("{} frames in {:.0} ms {}", n, t.elapsed().as_secs_f64() * 1e3, if r.is_ok() { "" } else { "ERR" });
    }
    println!("total {frames} frames in {:.2} s = {:.0} frames/s", t0.elapsed().as_secs_f64(), frames as f64 / t0.elapsed().as_secs_f64());
    Ok(())
}
