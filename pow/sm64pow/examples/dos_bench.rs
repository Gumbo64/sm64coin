//! What verifying costs, and what an attacker can mint for free (docs/DOS.md).
//!
//! `dos_bench <rom> <chain.jsonl> [variants]`: the chain is one JSON object per
//! block (height, id, prev, ts, seed, proof: hex), from `get_block`. It
//! replays the chain in order (as a rebuild: no time limit), timing each run;
//! then times rejecting an invalid claim (the same proof under another seed);
//! then, with `variants`, tries the cheap malleations of every block's proof
//! (last byte flipped or dropped) and counts the valid ones: each is a
//! different block id with the same seed and the same length.
use sm64pow::chain::{BlockVerdict, Parent, rebuild_block_opts};
use sm64pow::Seed;
use std::time::Instant;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}
fn id32(s: &str) -> [u8; 32] {
    hex(s).try_into().unwrap()
}

struct B {
    height: u64,
    id: [u8; 32],
    prev: [u8; 32],
    ts: u64,
    seed: Seed,
    proof: Vec<u8>,
}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    sm64pow::init(std::fs::read(&a[1])?)?;
    let variants = a.get(3).is_some_and(|x| x == "variants");
    let mut blocks = Vec::new();
    for l in std::fs::read_to_string(&a[2])?.lines() {
        let j: serde_json::Value = serde_json::from_str(l)?;
        blocks.push(B {
            height: j["height"].as_u64().unwrap(),
            id: id32(j["id"].as_str().unwrap()),
            prev: id32(j["prev"].as_str().unwrap()),
            ts: j["ts"].as_u64().unwrap(),
            seed: Seed::from_hex(j["seed"].as_str().unwrap())?,
            proof: hex(j["proof"].as_str().unwrap()),
        });
    }
    let ts: std::collections::HashMap<[u8; 32], u64> = blocks.iter().map(|b| (b.id, b.ts)).collect();
    let parent = |b: &B| -> Parent {
        if b.height == 1 { Parent::Genesis } else { Parent::Block { id: b.prev, gap_seconds: b.ts - ts[&b.prev] } }
    };
    let (mut tf, mut tt) = (0u64, 0f64);
    println!("height codec frames bytes seconds frames/s");
    for b in blocks.iter().filter(|b| b.height > 0) {
        let (frames, _, cid, _) = sm64pow::proof::split_blob(&b.proof)?;
        let t = Instant::now();
        let v = rebuild_block_opts(&b.id, parent(b), &b.seed, &b.proof, true)?;
        let s = t.elapsed().as_secs_f64();
        anyhow::ensure!(matches!(v, BlockVerdict::Valid { .. }), "block {} {v:?}", b.height);
        println!("{} {cid} {frames} {} {s:.3} {:.0}", b.height, b.proof.len(), frames as f64 / s);
        tf += frames as u64;
        tt += s;
    }
    println!("total {tf} frames in {tt:.2} s = {:.0} frames/s", tf as f64 / tt);

    // An invalid claim: the tip's proof under another seed (what an attacker
    // gets for free by changing the nonce). Costs the whole claimed length.
    let tip = blocks.last().unwrap();
    let mut bad = tip.seed;
    bad.0[0] ^= 1;
    let t = Instant::now();
    let v = rebuild_block_opts(&[0xee; 32], parent(tip), &bad, &tip.proof, true)?;
    let (frames, ..) = sm64pow::proof::split_blob(&tip.proof)?;
    println!("invalid claim ({frames} frames, wrong seed): {:.3} s, {v:?}", t.elapsed().as_secs_f64());
    // Where a replay's time goes: the emulator alone (stepping the played pads
    // from power-on) against the whole verification of the first block.
    {
        let b = &blocks[1];
        let pads = sm64pow::chain::played_block_opts(&b.id, parent(b), &b.seed, &b.proof, true)?.map_err(|v| anyhow::anyhow!("{v:?}"))?;
        let mut g = sm64pow::generator()?.create_game()?;
        let t = Instant::now();
        for p in &pads {
            g.step_game(*p)?;
        }
        let emu = t.elapsed().as_secs_f64();
        let t = Instant::now();
        // A new id: the played-pads cache misses, so this replays.
        let mut vid = b.id;
        vid[31] ^= 1;
        let v = sm64pow::chain::played_block_opts(&vid, parent(b), &b.seed, &b.proof, true)?;
        let full = t.elapsed().as_secs_f64();
        println!("block 1 ({} frames): emulator alone {:.3} s ({:.0} frames/s); replay with pokes, state codec and checks {:.3} s ({:.0} frames/s) {}", pads.len(), emu, pads.len() as f64 / emu, full, pads.len() as f64 / full, v.is_ok());
    }
    // The cheap pre-checks, for scale.
    let t = Instant::now();
    for _ in 0..100_000 {
        std::hint::black_box(sm64pow::proof::split_blob(std::hint::black_box(&tip.proof))?);
    }
    println!("proof_frames (split_blob): {:.2} us", t.elapsed().as_secs_f64() * 10.0);

    if a.get(3).is_some_and(|x| x == "sweep") {
        // Every value of the tip's last byte: how many valid variants one byte gives.
        let n = tip.proof.len();
        let (mut ok, t) = (0, Instant::now());
        for x in 0..=255u8 {
            if x == tip.proof[n - 1] {
                continue;
            }
            let mut p = tip.proof.clone();
            p[n - 1] = x;
            let mut vid = tip.id;
            vid[0] ^= 0x77;
            vid[1] = x;
            if matches!(rebuild_block_opts(&vid, parent(tip), &tip.seed, &p, true)?, BlockVerdict::Valid { .. }) {
                ok += 1;
            }
        }
        println!("tip: {ok} of 255 other last-byte values are valid variants ({:.1} s)", t.elapsed().as_secs_f64());
    }
    if variants {
        let mut valid_blocks = 0;
        let mut total_valid = 0;
        for b in blocks.iter().filter(|b| b.height > 0) {
            let mut tries: Vec<(String, Vec<u8>)> = Vec::new();
            let n = b.proof.len();
            for x in [0x01u8, 0x80, 0xff] {
                let mut p = b.proof.clone();
                p[n - 1] ^= x;
                tries.push((format!("last^{x:02x}"), p));
            }
            tries.push(("drop last".into(), b.proof[..n - 1].to_vec()));
            let mut ok = Vec::new();
            for (k, (what, p)) in tries.iter().enumerate() {
                let mut vid = b.id;
                vid[0] ^= 0x5a;
                vid[1] = k as u8;
                let t = Instant::now();
                let v = rebuild_block_opts(&vid, parent(b), &b.seed, p, true)?;
                if let BlockVerdict::Valid { frames, .. } = v {
                    let secs = t.elapsed().as_secs_f64();
                    // Run-level or encoding-level? Compare the decoded inputs.
                    let w0 = sm64pow::chain::played_wanted_block_opts(&b.id, parent(b), &b.seed, &b.proof, true)?.map(|p| p.wanted()).ok();
                    let w1 = sm64pow::chain::played_wanted_block_opts(&vid, parent(b), &b.seed, p, true)?.map(|p| p.wanted()).ok();
                    let diff = match (&w0, &w1) {
                        (Some(a), Some(c)) => {
                            let first = a.iter().zip(c.iter()).position(|(x, y)| x != y);
                            format!("inputs {}/{}, first differing input {:?} of {}", a.len(), c.len(), first, a.len())
                        }
                        _ => "?".into(),
                    };
                    ok.push(format!("{what} ({frames} frames, {secs:.2} s, proof_id differs: {}, {diff})", sm64pow::proof_id(p) != sm64pow::proof_id(&b.proof)));
                }
            }
            if !ok.is_empty() {
                valid_blocks += 1;
                total_valid += ok.len();
                println!("block {}: {} valid variant(s): {}", b.height, ok.len(), ok.join("; "));
            }
        }
        println!("{valid_blocks} of {} blocks have a valid last-byte variant ({total_valid} variants)", blocks.len() - 1);
    }
    Ok(())
}
