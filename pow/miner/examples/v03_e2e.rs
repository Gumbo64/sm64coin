//! v0.3 end-to-end check against a live daemon (docs/V0.3-NEW-CHAIN.md):
//! submits a block saved by `automine --save-block` (signed with the
//! template's key when it fits the limit), then what an attacker and an
//! equivocating miner could send for the same template, and prints the
//! daemon's answers.
//!
//!   cargo run --release -p sm64coin-miner --example v03_e2e -- SAVED.json http://127.0.0.1:RPC [submit]
//!
//! With `submit`, only submits the miner's signed block (when it fits).
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use sm64coin_miner::rpc::{Daemon, seal_with};
use sm64pow::sign::Keypair;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn submit(d: &Daemon, what: &str, blob: &str) {
    let t = Instant::now();
    let r = d.call("submit_block", json!([blob]));
    println!("{what}: {} ({:.1} ms)", match r {
        Ok(_) => "ACCEPTED".to_string(),
        Err(e) => format!("refused: {e}"),
    }, t.elapsed().as_secs_f64() * 1e3);
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&args[1])?)?;
    let d = Daemon::new(&args[2]);
    let s = |k: &str| saved[k].as_str().unwrap_or_default().to_string();
    let (tpl, seed) = (s("template_blob"), s("seed_hash"));
    let key = Keypair::from_secret(&<[u8; 32]>::try_from(hex::decode(s("key_secret"))?.as_slice())?).ok_or_else(|| anyhow!("no key"))?;
    let proof = hex::decode(s("proof"))?;
    let frames = saved["frames"].as_u64().unwrap() as u32;
    let parent_ts = saved["parent_ts"].as_u64().unwrap();
    let info = d.call("get_block_template", json!({"wallet_address": "SM64RtPqsHM63LZn2u9Myx9HtC4kjhu1h98SRtJz9USB7NZAoCMdZSBD7D1L2DWu9uCYZFUSVKytreuzEMJ6VjBU2tbjKqevFH"}))?;
    let retarget = info["sm64_retarget_difficulty"].as_u64().unwrap_or(1).max(1);
    while sm64pow::rules().max_frames(retarget, now().saturating_sub(parent_ts)) < frames {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let ts = now();
    println!("block of {frames} frames, miner key {}", hex::encode(key.public));
    if args.get(3).map(|s| s.as_str()) == Some("submit") {
        submit(&d, "the miner's signed block", &seal_with(&tpl, &seed, &key, &proof, Some(ts))?);
        return Ok(());
    }
    // 1. An unsigned block (v0.2 style: zero signature), and one signed by another key.
    let tplb = hex::decode(&tpl)?;
    let unsigned = hex::encode(sm64pow::mining::seal_block(&tplb, &proof, &[0; 64], Some(ts))?);
    submit(&d, "unsigned (zero sm64_sig)", &unsigned);
    let other = Keypair::from_entropy(&[0x55; 32]).unwrap();
    submit(&d, "signed by a key the coinbase doesn't commit", &seal_with(&tpl, &seed, &other, &proof, Some(ts))?);
    // 2. The real block.
    submit(&d, "the miner's signed block", &seal_with(&tpl, &seed, &key, &proof, Some(ts))?);
    let sig = sm64pow::mining::sign_proof(&key, &sm64pow::Seed::from_hex(&seed)?.0, &proof)?;
    // 3. A third party's variants (the cheap last-byte edits) with the miner's signature copied.
    let mut edits: Vec<(String, Vec<u8>)> = [0x01u8, 0x80, 0xff]
        .iter()
        .map(|x| {
            let mut v = proof.clone();
            *v.last_mut().unwrap() ^= x;
            (format!("last byte ^ {x:02x}"), v)
        })
        .collect();
    edits.push(("last byte dropped".into(), proof[..proof.len() - 1].to_vec()));
    for (name, v) in &edits {
        submit(&d, &format!("third-party variant ({name}), copied signature"), &hex::encode(sm64pow::mining::seal_block(&tplb, v, &sig, Some(ts))?));
    }
    // 4. The miner itself signs one of the same length: equivocation evidence.
    for (name, v) in &edits[..1] {
        submit(&d, &format!("the miner's own equal-length variant ({name}), signed"), &seal_with(&tpl, &seed, &key, v, Some(ts))?);
    }
    let t = d.call("get_block_template", json!({"wallet_address": "SM64RtPqsHM63LZn2u9Myx9HtC4kjhu1h98SRtJz9USB7NZAoCMdZSBD7D1L2DWu9uCYZFUSVKytreuzEMJ6VjBU2tbjKqevFH"}))?;
    let tip = d.call("get_last_block_header", json!({}))?;
    println!(
        "tip {} at height {}; next template builds on {} (height {})",
        tip["block_header"]["hash"].as_str().unwrap_or_default(),
        tip["block_header"]["height"],
        t["prev_hash"].as_str().unwrap_or_default(),
        t["height"]
    );
    Ok(())
}
