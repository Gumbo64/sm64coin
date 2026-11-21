//! Replays a Mupen64 movie (.m64) as the wanted input and shows how the
//! perturbation breaks it: the played pads differ from the movie's on poked
//! frames, and even one differing frame sends the run elsewhere.
//!
//!   cargo run --release -p sm64coin-sdk --example replay_bot -- [route.m64] [seed]
use sm64coin_sdk::*;

struct ReplayBot {
    route: Vec<GamePad>,
    poked: usize,
    first_poke: Option<usize>,
}

impl Bot for ReplayBot {
    fn play(&mut self, env: &mut MiningEnv, stop: &dyn Fn() -> bool) -> anyhow::Result<()> {
        self.poked = 0;
        self.first_poke = None;
        // Past the end of the movie: hold neutral.
        while !env.is_success() && !env.is_over() && !stop() {
            let f = env.frame();
            let wanted = self.route.get(f).copied().unwrap_or_default();
            let played = env.step(wanted)?;
            if played != wanted {
                self.poked += 1;
                self.first_poke.get_or_insert(f);
            }
        }
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().cloned().unwrap_or("pow/miner/routes/block1-human.m64".into());
    let seed: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    init_rom("")?;
    let route = load_m64(&path)?;
    println!("{path}: {} frames", route.len());

    // The route as recorded does reach a star when nothing perturbs it.
    // (Reference: replay with the perturbation off, i.e. a plain game.)
    let g = sm64pow::generator()?.create_game()?;
    let mut g = g;
    for (i, p) in route.iter().enumerate() {
        g.step_game(*p)?;
        if g.get_game_state()?.num_stars > 0 {
            println!("unperturbed: star at frame {}", i + 1);
            break;
        }
    }

    let mut bot = ReplayBot { route, poked: 0, first_poke: None };
    let r = practice(&mut bot, seed, 1, &|| false)?;
    println!(
        "perturbed (seed {seed}): {} poked frames, first at frame {:?}; ran {} frames, star={}",
        bot.poked, bot.first_poke, r.frames, r.success
    );
    Ok(())
}
