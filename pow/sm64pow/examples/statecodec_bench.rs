//! statecodec_bench RUN.json... : proof sizes per codec and replay timings.
use sm64pow::proof::{self, pad_from, Proof, Start};
use sm64pow::sm64_binds::GamePad;
use sm64pow::statecodec::{self, StateCtx1};
use std::time::Instant;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn main() -> anyhow::Result<()> {
    sm64pow::init_default()?;
    let p = sm64pow::params(1);
    println!("{:<10} {:>6} {:>7} | {:>6} {:>6} {:>6} {:>7} | bits/frame raw-xz  state | replay ms: stateless  state  (+%)", "run", "frames", "inputs", "raw", "rle", "xz", "state");
    let mut tot = [0usize; 4];
    let mut frames_all = 0usize;
    for path in std::env::args().skip(1) {
        let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        let pads = |k: &str| -> Vec<GamePad> { hex(j[k].as_str().unwrap_or("")).chunks_exact(4).map(pad_from).collect() };
        let start = if j["start"].as_u64() == Some(1) { Start::PowerOn } else { Start::Parent };
        let seed = sm64pow::Seed::from_hex(j["seed_hash"].as_str().unwrap())?;
        let pr = Proof { frames: j["frames"].as_u64().unwrap() as u32, start, inputs: pads("inputs") };
        let ps = if start == Start::Parent { Some(statecodec::state_after(&pads("parent_played"))?) } else { None };
        let blob = statecodec::encode_proof_state(&StateCtx1, ps.as_ref(), &seed, &p, &pr)?;
        let sizes = [proof::encode_proof_with(&proof::Raw, &pr).len(), proof::encode_proof_with(&proof::RunLength, &pr).len(), proof::encode_proof_with(&proof::Xz, &pr).len(), blob.len()];
        for i in 0..4 {
            tot[i] += sizes[i];
        }
        frames_all += pr.frames as usize;
        // Replay timings (best of 3): stateless input feed vs decode + canonical re-encode.
        let (mut t_plain, mut t_state) = (f64::MAX, f64::MAX);
        for _ in 0..3 {
            let (mut g, _) = sm64pow::chain::mining_start(start, ps.as_ref(), &seed, &p)?;
            let t = Instant::now();
            let mut next = 0;
            for _ in 0..pr.frames {
                let (_, poked) = g.step(pr.inputs.get(next).copied().unwrap_or_default())?;
                if !poked { next += 1; }
            }
            t_plain = t_plain.min(t.elapsed().as_secs_f64());
            let (_, start2, cid, body) = proof::split_blob(&blob)?;
            let (mut g, _) = sm64pow::chain::mining_start(start2, ps.as_ref(), &seed, &p)?;
            let c = statecodec::state_codec(cid).unwrap();
            let (mut dec, mut enc) = (c.decoder(body), c.encoder());
            let t = Instant::now();
            for _ in 0..pr.frames {
                let (_, poked) = g.step_with(|f| { let x = dec.pad(f); enc.pad(f, x); Ok(x) })?;
                if poked { dec.poked(); enc.poked(); }
            }
            assert_eq!(enc.finish(), body);
            t_state = t_state.min(t.elapsed().as_secs_f64());
        }
        let b = |x: usize| x as f64 * 8.0 / pr.frames as f64;
        println!("{:<10} {:>6} {:>7} | {:>6} {:>6} {:>6} {:>7} | {:>8.3} {:>6.3} | {:>8.1} {:>8.1} ({:+.1}%)",
            std::path::Path::new(&path).file_stem().unwrap().to_string_lossy(), pr.frames, pr.inputs.len(), sizes[0], sizes[1], sizes[2], sizes[3], b(sizes[2]), b(sizes[3]), t_plain * 1e3, t_state * 1e3, (t_state / t_plain - 1.0) * 100.0);
    }
    println!("TOTAL      {:>6}         | {:>6} {:>6} {:>6} {:>7} | bits/frame xz {:.3} state {:.3}", frames_all, tot[0], tot[1], tot[2], tot[3], tot[2] as f64 * 8.0 / frames_all as f64, tot[3] as f64 * 8.0 / frames_all as f64);
    Ok(())
}
