//! The smallest possible bot: random inputs. It will essentially never reach a
//! star; it exists to show the API.
//!
//!   cargo run --release -p sm64coin-sdk --example random_bot                # offline practice
//!   cargo run --release -p sm64coin-sdk --example random_bot -- http://127.0.0.1:16481 SM64...   # mine
use rand::{Rng, SeedableRng, rngs::SmallRng};
use sm64coin_sdk::*;

struct RandomBot(SmallRng);

impl Bot for RandomBot {
    fn play(&mut self, env: &mut MiningEnv, stop: &dyn Fn() -> bool) -> anyhow::Result<()> {
        while !env.is_success() && !env.is_over() && !stop() {
            let wanted = GamePad::new(
                if self.0.random_bool(0.3) { buttons::A_BUTTON } else { 0 },
                self.0.random_range(-80..=80),
                self.0.random_range(-80..=80),
            );
            // `played` differs from `wanted` on frames the network pokes.
            let _played = env.step(wanted)?;
        }
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut bot = RandomBot(SmallRng::seed_from_u64(1));
    if args.len() >= 2 {
        return run_miner(&mut bot, &args[0], &args[1], &args.get(2).cloned().unwrap_or_default());
    }
    init_rom("")?;
    let r = practice(&mut bot, 1, 1, &|| false)?;
    println!("{} of {} frames, success={}, {:.0} frames/s", r.frames, r.max_frames, r.success, r.fps());
    Ok(())
}
