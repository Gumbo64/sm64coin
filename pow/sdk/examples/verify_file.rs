//! Checks a proof written by any implementation (Python, your own code)
//! with the consensus verifier.
//!
//!   verify_file <seed_hash_hex> <difficulty> <pads_raw_file> [--conformance] [--rom PATH]
//!
//! The file is raw pads, 4 bytes a frame: u16 little-endian buttons, i8 stick
//! x, i8 stick y. Exit status 0 when the proof is VALID. With --conformance
//! the run need not reach a star: it only checks that every pad is the one the
//! perturbation allows (exit 0 if so), which tests an implementation's
//! reseeding and rng_pad handling on runs that don't win.
use sm64coin_sdk::*;

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() == 2 && a[0] == "--params" {
        // For cross-checking other implementations' copies of the rules.
        let p = sm64pow::params(a[1].parse()?);
        println!("{} {} {} {} {}", p.max_frames, p.rng.random_amount, p.rng.window_length, p.rng.random_burst_length, p.reseed_frames);
        return Ok(());
    }
    if a.len() < 3 {
        anyhow::bail!("usage: verify_file <seed_hash_hex> <difficulty> <pads_raw_file> [--conformance] [--rom PATH]");
    }
    let rom = a.iter().position(|x| x == "--rom").and_then(|i| a.get(i + 1)).cloned().unwrap_or_default();
    init_rom(&rom)?;
    let seed = Seed::from_hex(&a[0])?;
    let d: u64 = a[1].parse()?;
    let raw = std::fs::read(&a[2])?;
    let pads: Vec<GamePad> = raw.chunks_exact(4).map(proof::pad_from).collect();
    if a.iter().any(|x| x == "--conformance") {
        let p = sm64pow::params(d);
        let mut g = sm64pow::new_mining_game(&seed, &p)?;
        for (i, pad) in pads.iter().enumerate() {
            sm64pow::reseed(&mut g, &seed, &p, i)?;
            let allowed = g.rng_pad(*pad)?;
            if allowed != *pad {
                println!("NONCONFORMING at frame {i}: {pad:?} but the perturbation forces {allowed:?}");
                std::process::exit(1);
            }
            g.step_game(*pad)?;
        }
        println!("CONFORMING: all {} pads are what the perturbation allows", pads.len());
        return Ok(());
    }
    let p = sm64pow::params(d);
    match sm64pow::verify_pads(&seed, &p, &pads)? {
        Verdict::Valid { frames } => println!("VALID {frames} frames"),
        Verdict::Invalid(w) => {
            println!("INVALID {w}");
            std::process::exit(1)
        }
    }
    Ok(())
}
