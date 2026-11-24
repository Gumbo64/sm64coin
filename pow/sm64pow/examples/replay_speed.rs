//! Emulation speed of a replay: `replay_speed <rom> <played pads hex file>` steps the pads
//! (a cycle's played pads from get_sm64_cycle) three ways and prints frames per second.
use sm64pow::sm64_binds::GamePad;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    sm64pow::init(std::fs::read(&a[1])?)?;
    let hex = std::fs::read_to_string(&a[2])?;
    let b: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap()).collect();
    let pads: Vec<GamePad> = b.chunks(4).map(|c| GamePad::new(u16::from_le_bytes([c[0], c[1]]), c[2] as i8, c[3] as i8)).collect();
    for mode in ["step only", "step + regions (perturbation)", "step + regions + progress"] {
        let mut g = sm64pow::generator()?.create_game()?;
        let t = Instant::now();
        for p in &pads {
            if mode != "step only" {
                let _ = sm64pow::game::read_region_bytes(&g)?;
            }
            g.step_game(*p)?;
            if mode != "step only" {
                let _ = sm64pow::game::read_region_bytes(&g)?;
            }
            if mode == "step + regions + progress" {
                let _ = sm64pow::game::read_progress(&g)?;
            }
        }
        let s = t.elapsed().as_secs_f64();
        println!("{mode}: {} frames in {s:.2} s = {:.0} frames/s", pads.len(), pads.len() as f64 / s);
    }
    // A fresh game per block, and loading a parent's end state into it.
    let t = Instant::now();
    let mut g = sm64pow::generator()?.create_game()?;
    for _ in 0..9 {
        g = sm64pow::generator()?.create_game()?;
    }
    println!("create_game: {:.1} ms each", t.elapsed().as_secs_f64() * 100.0);
    for p in &pads[..20000] {
        g.step_game(*p)?;
    }
    let st = g.save_state();
    let t = Instant::now();
    for _ in 0..10 {
        let mut h = sm64pow::generator()?.create_game()?;
        h.load_state(&st)?;
    }
    println!("create_game + load_state: {:.1} ms each", t.elapsed().as_secs_f64() * 100.0);
    let t = Instant::now();
    for _ in 0..10 {
        let _ = g.save_state();
    }
    println!("save_state: {:.1} ms each", t.elapsed().as_secs_f64() * 100.0);
    Ok(())
}
