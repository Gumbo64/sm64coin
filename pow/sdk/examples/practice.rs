//! Offline practice: runs a bot on random local seeds (no daemon) and reports
//! frames, success and the consensus verifier's verdict.
//!
//!   cargo run --release -p sm64coin-sdk --example practice -- [--seeds N] [--difficulty D] [--first-seed S] [--bot random|replay:ROUTE.m64]
//!
//! Edit `make_bot` to try your own bot.
use rand::{Rng, SeedableRng, rngs::SmallRng};
use sm64coin_sdk::*;

struct Random(SmallRng);
impl Bot for Random {
    fn play(&mut self, env: &mut MiningEnv, stop: &dyn Fn() -> bool) -> anyhow::Result<()> {
        while !env.is_over() && !stop() {
            let w = GamePad::new(if self.0.random_bool(0.3) { buttons::A_BUTTON } else { 0 }, self.0.random_range(-80..=80), self.0.random_range(-80..=80));
            env.step(w)?;
        }
        Ok(())
    }
}

struct Replay(Vec<GamePad>);
impl Bot for Replay {
    fn play(&mut self, env: &mut MiningEnv, stop: &dyn Fn() -> bool) -> anyhow::Result<()> {
        while !env.is_over() && !stop() {
            let w = self.0.get(env.frame()).copied().unwrap_or_default();
            env.step(w)?;
        }
        Ok(())
    }
}

fn make_bot(spec: &str) -> anyhow::Result<Box<dyn Bot>> {
    match spec.strip_prefix("replay:") {
        Some(p) => Ok(Box::new(Replay(load_m64(p)?))),
        None if spec == "random" => Ok(Box::new(Random(SmallRng::seed_from_u64(7)))),
        None => anyhow::bail!("unknown bot {spec}"),
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str, d: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned().unwrap_or(d.to_string());
    let (n, d, first): (u64, u64, u64) = (get("--seeds", "5").parse()?, get("--difficulty", "1").parse()?, get("--first-seed", "1").parse()?);
    let spec = get("--bot", "random");
    init_rom("")?;
    let mut bot = make_bot(&spec)?;
    let mut wins = 0;
    for seed in first..first + n {
        let r = practice(bot.as_mut(), seed, d, &|| false)?;
        let verdict = match &r.verdict {
            Some(Verdict::Valid { frames }) => format!("VALID ({frames} frames, {} byte proof)", r.proof.len()),
            Some(Verdict::Invalid(w)) => format!("INVALID: {w}"),
            None => "no star".into(),
        };
        println!("seed {seed:>4} d={d}: {:>6}/{} frames, {:>6.0} frames/s  {verdict}", r.frames, r.max_frames, r.fps());
        wins += r.success as u64;
    }
    println!("{wins}/{n} stars");
    Ok(())
}
