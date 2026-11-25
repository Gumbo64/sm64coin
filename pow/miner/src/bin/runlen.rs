//! runlen: measures best run length vs search compute (research tool, submits nothing).
//! `runlen --parent <hash|top> --seed-n N --searches K [--guide-weight W] --out file.jsonl`
//! Runs K rrt searches from the parent's end state under the perturbation seed derived from
//! `--seed-n` (a different n is a different template's pokes), search seeds `--sseed0 + k`.
//! With --guide-weight > 0 search k is guided by the best run so far (automine's anytime mode).
use anyhow::{Result, bail};
use clap::Parser;
use sm64coin_miner::{rh, rpc, rrt};
use sm64pow::proof::Start;
use sm64pow::sm64_binds::SaveState;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:29781")]
    daemon: String,
    #[arg(long, default_value = "baserom.us.z64")]
    rom: String,
    /// Parent block hash ("top": the tip; "power-on": start from power-on).
    #[arg(long, default_value = "top")]
    parent: String,
    /// Cache the parent's end state here (raw bytes) so the daemon is asked once.
    #[arg(long)]
    state_file: Option<String>,
    #[arg(long, default_value_t = 0)]
    seed_n: u64,
    #[arg(long, default_value_t = 1)]
    searches: u64,
    #[arg(long, default_value_t = 0)]
    sseed0: u64,
    #[arg(long, default_value_t = 1)]
    threads: usize,
    #[arg(long, default_value_t = 60_000_000)]
    hub_frames: u64,
    #[arg(long, default_value_t = 150_000_000)]
    course_frames: u64,
    #[arg(long, default_value_t = 8)]
    checkpoints: usize,
    #[arg(long, default_value_t = 1)]
    retarget: u64,
    #[arg(long, default_value_t = 0.0)]
    guide_weight: f64,
    #[arg(long, default_value = "run/runlen-scratch")]
    scratch: String,
    #[arg(long, default_value = "")]
    env: String,
    #[arg(long)]
    tag: Option<String>,
    #[arg(long)]
    out: String,
}

fn main() -> Result<()> {
    let a = Args::parse();
    sm64pow::init(std::fs::read(&a.rom)?)?;
    let d = rpc::Daemon::new(&a.daemon);
    let (start, parent): (Start, Option<SaveState>) = if a.parent == "power-on" {
        (Start::PowerOn, None)
    } else {
        let bytes = match a.state_file.as_ref().filter(|f| std::path::Path::new(f).exists()) {
            Some(f) => std::fs::read(f)?,
            None => {
                let h = if a.parent == "top" { d.top_hash()? } else { a.parent.clone() };
                let s = d.end_state(&h)?;
                let b = s.as_bytes().to_vec();
                if let Some(f) = &a.state_file { std::fs::write(f, &b)?; }
                b
            }
        };
        (Start::Parent, Some(SaveState::from_bytes(bytes)?))
    };
    let seedhex = format!("{:016x}{:016x}{:016x}{:016x}", a.seed_n.wrapping_mul(0x9E3779B97F4A7C15) ^ 0x1234, a.seed_n.wrapping_add(77).wrapping_mul(0xC2B2AE3D27D4EB4F), !a.seed_n, a.seed_n ^ 0xdeadbeef);
    let seed = sm64pow::Seed::from_hex(&seedhex)?;
    let params = sm64pow::params(a.retarget.max(1));
    let never = Arc::new(AtomicBool::new(false));
    std::fs::create_dir_all(&a.scratch)?;
    let mut best: Option<rh::Mined> = None;
    let mut cum: u64 = 0;
    let t0 = std::time::Instant::now();
    for k in 0..a.searches {
        let mut env = a.env.clone();
        if a.guide_weight > 0.0 {
            if let Some(b) = &best {
                if !b.keys.is_empty() {
                    let mut u = b.keys.clone(); u.sort(); u.dedup();
                    let p = std::path::Path::new(&a.scratch).join("guide_keys.txt");
                    std::fs::write(&p, u.join("\n"))?;
                    env = format!("{env} OBRRT_GUIDE_KEYS={} OBRRT_GUIDE_WEIGHT={}", p.display(), a.guide_weight);
                }
            }
        }
        let ro = rrt::RrtOptions {
            threads: a.threads, hub_frames: a.hub_frames, course_frames: a.course_frames, checkpoints: a.checkpoints,
            extra_env: env, seed: (a.sseed0 + k).wrapping_mul(104729).wrapping_add(7919), frame_cap: 0,
            scratch: a.scratch.clone().into(), rom: Some(a.rom.clone()), ..Default::default()
        };
        let t1 = std::time::Instant::now();
        let r = rrt::mine(start, parent.as_ref(), &seed, &params, &ro, &never, |_| {})?;
        let secs = t1.elapsed().as_secs_f64();
        let (frames, sim, status) = match r {
            rh::Attempt::Found(m) => {
                let f = m.pads.len() as u64;
                let s = m.sim_frames;
                if best.as_ref().is_none_or(|b| (m.pads.len()) < b.pads.len()) { best = Some(m); }
                (Some(f), s, "found")
            }
            rh::Attempt::Gave(_) => (None, 0, "gave"),
            rh::Attempt::NoRoute(_) => (None, 0, "noroute"),
        };
        cum += sim;
        let line = serde_json::json!({"tag": a.tag, "parent": a.parent, "seed_n": a.seed_n, "k": k, "sseed": a.sseed0 + k, "guide": a.guide_weight, "status": status, "frames": frames, "sim_frames": sim, "cum_sim_frames": cum, "secs": secs, "cum_secs": t0.elapsed().as_secs_f64(), "threads": a.threads, "best": best.as_ref().map(|b| b.pads.len())});
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&a.out)?;
        writeln!(f, "{line}")?;
        if status == "noroute" { bail!("no route"); }
    }
    Ok(())
}
