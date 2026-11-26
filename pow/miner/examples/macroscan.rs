//! Measures the boot and settle macros over many seeds (the perturbation's pokes differ per seed).
//! Usage: macroscan <boot|settle> <legacy|fast> <seeds> [threads=4] [state.gz for settle]
//! Prints the frames at which Mario is first controllable and at which the macro hands over, with
//! mean / p50 / p95 / worst, and the failures by kind.
use obrrt_core::{GamePad, read_state};
use rayon::prelude::*;
use sm64coin_miner::rh;
use sm64pow::proof::Start;
use sm64pow::{Seed, params};
use std::io::Read;

fn seed(i: u64) -> Seed {
    let mut b = [0u8; 32];
    for (k, x) in b.iter_mut().enumerate() {
        *x = ((i * 2654435761 + k as u64 * 40503).wrapping_mul(0x9E3779B97F4A7C15) >> 56) as u8;
    }
    Seed(b)
}

#[derive(Default, Clone)]
struct Res {
    first_ctl: usize,
    ready: usize,
    fail: Option<String>,
}

fn pct(v: &[usize], p: f64) -> usize {
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn main() -> anyhow::Result<()> {
    sm64pow::init_default()?;
    let a: Vec<String> = std::env::args().collect();
    let (mode, policy, n) = (a[1].clone(), a[2].clone(), a[3].parse::<u64>()?);
    let threads: usize = a.get(4).map(|x| x.parse().unwrap()).unwrap_or(4);
    let first: u64 = std::env::var("FIRST").ok().map(|x| x.parse().unwrap()).unwrap_or(0);
    let state = match a.get(5) {
        Some(p) => {
            let gz = std::fs::read(p)?;
            let mut bytes = Vec::new();
            flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut bytes)?;
            Some(sm64pow::sm64_binds::SaveState::from_bytes(bytes)?)
        }
        None => None,
    };
    let legacy = policy == "legacy";
    let boot = mode == "boot";
    let p = params(5);
    let max = 6000usize;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build()?;
    let res: Vec<Res> = pool.install(|| {
        (first..first + n).into_par_iter().map(|i| {
            let mut r = Res::default();
            let (mut game, before) = if boot { sm64pow::chain::mining_start(Start::PowerOn, None, &seed(i), &p).unwrap() } else { sm64pow::chain::mining_start(Start::Parent, state.as_ref(), &seed(i), &p).unwrap() };
            // POKES=<bursts per 100 ticks> [BURST=<length>]: a harsher perturbation than the rules' (5 bursts of 2).
            if let Ok(k) = std::env::var("POKES") {
                let burst: u32 = std::env::var("BURST").ok().map(|x| x.parse().unwrap()).unwrap_or(2);
                let c = sm64pow::game::PokeConfig { window_length: 100, random_amount: k.parse().unwrap(), burst_length: burst, a_prob: 0.5, b_prob: 0.5, z_prob: 0.2, timing_mix: 100, pad_mix: 100 };
                game.pert = sm64pow::game::Perturber::new(&seed(i).0, &c);
            }
            let mut played: Vec<GamePad> = Vec::new();
            let out = match (boot, legacy) {
                (true, true) => rh::boot_legacy(&mut game, &mut played, max),
                (true, false) => sm64coin_miner::macros::boot(&mut game, &mut played, max),
                (false, true) => rh::settle_legacy(&mut game, &mut played, max),
                (false, false) => sm64coin_miner::macros::settle(&mut game, &mut played, max),
            };
            r.ready = played.len();
            // First controllable frame: replay the played pads on a plain game.
            let mut g = sm64pow::generator().unwrap().create_game().unwrap();
            if let Some(s) = state.as_ref().filter(|_| !boot) { g.load_state(s).unwrap(); }
            let mut quit = false; let mut line_seen = 0i8; let mut chosen = 0i8;
            r.first_ctl = usize::MAX;
            for (k, pad) in played.iter().enumerate() {
                let st = read_state(&g, k as u32, false);
                if st.controllable() && r.first_ctl == usize::MAX { r.first_ctl = k; }
                if !boot && st.level == 1 { quit = true; }
                if st.in_save_menu() && st.dialog_state == 1 { line_seen = st.menu_line; }
                else if line_seen != 0 { chosen = line_seen; line_seen = 0; }
                g.step_game(*pad).unwrap();
            }
            if read_state(&g, played.len() as u32, false).controllable() && r.first_ctl == usize::MAX { r.first_ctl = played.len(); }
            match out {
                Err(e) => r.fail = Some(format!("error: {}", e.to_string().chars().take(60).collect::<String>())),
                Ok(st) => {
                    let end = read_state(&g, played.len() as u32, false);
                    let stars_before = before.stars as i16;
                    if !end.controllable() { r.fail = Some("not controllable at the end".into()); }
                    else if boot && (end.level != 16 || end.stars != 0) { r.fail = Some(format!("boot ended in level {} stars {}", end.level, end.stars)); }
                    else if !boot && end.stars != stars_before { r.fail = Some(format!("star not kept: {} -> {} (option 3 / lost)", stars_before, end.stars)); }
                    else if !boot && (quit || chosen == 2) { r.fail = Some(format!("option {chosen}: save-and-quit (recovered via the title screen)")); }
                    else if !boot && chosen != 1 { r.fail = Some(format!("option {chosen}")); }
                    let _ = st;
                }
            }
            r
        }).collect()
    });
    let mut ready: Vec<usize> = res.iter().filter(|r| r.fail.as_ref().map_or(true, |f| !f.starts_with("error"))).map(|r| r.ready).collect();
    let mut fc: Vec<usize> = res.iter().filter(|r| r.first_ctl != usize::MAX).map(|r| r.first_ctl).collect();
    ready.sort();
    fc.sort();
    let mean = |v: &[usize]| v.iter().sum::<usize>() as f64 / v.len().max(1) as f64;
    println!("{mode} {policy} seeds {first}..{} ({} ok)", first + n, ready.len());
    if !fc.is_empty() { println!("  first controllable: mean {:.1} p50 {} p95 {} worst {} best {}", mean(&fc), pct(&fc, 0.5), pct(&fc, 0.95), fc.last().unwrap(), fc[0]); }
    if !ready.is_empty() { println!("  handed over at:     mean {:.1} p50 {} p95 {} worst {} best {}", mean(&ready), pct(&ready, 0.5), pct(&ready, 0.95), ready.last().unwrap(), ready[0]); }
    let mut kinds = std::collections::BTreeMap::<String, (usize, Vec<u64>)>::new();
    for (i, r) in res.iter().enumerate() {
        if let Some(f) = &r.fail { let e = kinds.entry(f.clone()).or_default(); e.0 += 1; if e.1.len() < 8 { e.1.push(first + i as u64); } }
    }
    println!("  failures: {}", res.iter().filter(|r| r.fail.is_some()).count());
    for (k, (c, ex)) in kinds { println!("    {c} x {k} (seeds {ex:?})"); }
    Ok(())
}
