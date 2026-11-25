//! mix2_eval [chain N] [m64 MAXFRAMES PATH...] : size and decode speed of the state codecs 3-7
//! (codec crate only, one thread; the replay is done once to get the rows).
//!   chain N        the first N blocks of tests/fixtures/mainnet_v02.jsonl (miner runs, each replayed from its
//!                  parent's end state), one corpus row per block, plus their total
//!   m64 MAX PATH.. the first MAX frames of power-on movies (unperturbed), e.g. 120_2026.m64
//! Needs a ROM (SM64COIN_ROM or the default path).
use sm64pow::chain::{Parent, played_wanted_block};
use sm64pow::game::{Features, read_features, to_pad};
use sm64pow::proof::{Proof, Start};
use sm64pow::statecodec::{Pad, Run, state_codec};
use sm64pow::sm64_binds::GamePad;
use std::time::Instant;

type Rows = Vec<(Features, Pad, bool)>;

fn unhex(h: &str) -> Vec<u8> {
    (0..h.len() / 2).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn encode(id: u8, rows: &Rows) -> (Vec<u8>, f64) {
    let t = Instant::now();
    let mut e = state_codec(id).unwrap().encoder();
    for (f, p, poked) in rows {
        if *poked { e.poked() } else { e.pad(f, *p) }
    }
    let b = e.finish();
    (b, t.elapsed().as_secs_f64())
}

fn decode_secs(id: u8, body: &[u8], rows: &Rows) -> f64 {
    let t = Instant::now();
    let mut d = state_codec(id).unwrap().decoder(body);
    for (f, p, poked) in rows {
        if *poked { d.poked() } else { assert_eq!(d.pad(f), *p, "codec {id}") }
    }
    assert_eq!(d.finish(), body);
    t.elapsed().as_secs_f64()
}

fn main() -> anyhow::Result<()> {
    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    sm64pow::init(std::fs::read(rom)?)?;
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut corpora: Vec<(String, Vec<Rows>)> = vec![];
    let mut i = 0;
    while i < a.len() {
        match a[i].as_str() {
            "chain" => {
                let n: usize = a[i + 1].parse()?;
                i += 2;
                let mut prev: Option<([u8; 32], u64)> = None;
                let mut cum: Vec<GamePad> = vec![];
                let mut runs = vec![];
                for l in include_str!("../tests/fixtures/mainnet_v02.jsonl").lines().take(n) {
                    let j: serde_json::Value = serde_json::from_str(l)?;
                    let id: [u8; 32] = unhex(j["id"].as_str().unwrap()).try_into().unwrap();
                    let ts = j["ts"].as_u64().unwrap();
                    let seed = sm64pow::Seed::from_hex(j["seed"].as_str().unwrap())?;
                    let blob = unhex(j["proof"].as_str().unwrap());
                    let parent = prev.map_or(Parent::Genesis, |(pid, pts)| Parent::Block { id: pid, gap_seconds: ts - pts });
                    let pl = played_wanted_block(&id, parent, &seed, &blob)?.map_err(|v| anyhow::anyhow!("{v:?}"))?;
                    let (frames, start, _, _) = sm64pow::proof::split_blob(&blob)?;
                    let run = Run { seed, parent_played: if start == Start::Parent { cum.clone() } else { vec![] }, proof: Proof { frames, start, inputs: pl.wanted() } };
                    if start == Start::PowerOn {
                        cum.clear();
                    }
                    cum.extend(pl.pads.iter().copied());
                    runs.push(run.dump_rows()?.into_iter().map(|(f, p, k)| (f, to_pad(p), k)).collect());
                    prev = Some((id, ts));
                }
                corpora.push((format!("chain({n})"), runs));
            }
            "m64" => {
                let max: usize = a[i + 1].parse()?;
                i += 2;
                while i < a.len() && !["chain", "m64"].contains(&a[i].as_str()) {
                    let b = std::fs::read(&a[i])?;
                    let body = if b.len() >= 0x400 && &b[..4] == b"M64\x1a" { &b[0x400..] } else { &b[..] };
                    let pads: Vec<GamePad> = body.chunks_exact(4).take(max).map(|c| GamePad::new(u16::from_be_bytes([c[0], c[1]]), c[2] as i8, c[3] as i8)).collect();
                    let mut g = sm64pow::generator()?.create_game()?;
                    let mut rows = Vec::with_capacity(pads.len());
                    for p in &pads {
                        rows.push((read_features(&g)?, to_pad(*p), false));
                        g.step_game(*p)?;
                    }
                    corpora.push((std::path::Path::new(&a[i]).file_name().unwrap().to_string_lossy().to_string(), vec![rows]));
                    i += 1;
                }
            }
            x => anyhow::bail!("unknown arg {x}"),
        }
    }
    let ids = [3u8, 4, 5, 6, 7];
    println!("{:<22} {:>8} | bytes/bits-per-frame per codec 3 4 5 6 7 | best-of 4-7 (per run) | vs codec 5", "corpus", "frames");
    for (name, runs) in &corpora {
        let frames: usize = runs.iter().map(|r| r.len()).sum();
        let mut tot = [0usize; 5];
        let mut best = 0usize;
        let (mut dec_s, mut enc_s) = ([0f64; 5], [0f64; 5]);
        for rows in runs {
            let mut sz = [0usize; 5];
            for (k, &id) in ids.iter().enumerate() {
                let (b, es) = encode(id, rows);
                let ds = decode_secs(id, &b, rows);
                sz[k] = b.len();
                tot[k] += b.len();
                enc_s[k] += es;
                dec_s[k] += ds;
            }
            best += sz[1..].iter().min().unwrap();
        }
        let bpf = |x: usize| x as f64 * 8.0 / frames as f64;
        println!(
            "{:<22} {:>8} | {} | {} ({:.3}) | codec 6 {:+.1}%, codec 7 {:+.1}%, best-of {:+.1}%",
            name, frames,
            tot.iter().map(|&t| format!("{t} ({:.3})", bpf(t))).collect::<Vec<_>>().join("  "),
            best, bpf(best),
            (tot[3] as f64 / tot[2] as f64 - 1.0) * 100.0, (tot[4] as f64 / tot[2] as f64 - 1.0) * 100.0, (best as f64 / tot[2] as f64 - 1.0) * 100.0
        );
        println!("{:<22} {:>8} | decode k frames/s 3..7: {}  | encode: {}", "", "", dec_s.iter().map(|s| format!("{:.0}", frames as f64 / s / 1e3)).collect::<Vec<_>>().join(" "), enc_s.iter().map(|s| format!("{:.0}", frames as f64 / s / 1e3)).collect::<Vec<_>>().join(" "));
    }
    Ok(())
}
