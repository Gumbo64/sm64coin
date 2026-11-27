//! `cargo run --release -p sm64coin-miner --example campaign -- <daemon-url> <address> <rom> <data-dir> [threads] [seconds]`
//!
//! The library campaign (sm64coin_miner::campaign) on its own: prints its events and stats for `seconds` (0: until a block
//! is accepted), then stops it. What an app does, minus the UI.
use sm64coin_miner::campaign::{Campaign, CampaignConfig, Event};

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    anyhow::ensure!(a.len() >= 5, "usage: campaign <daemon-url> <address> <rom> <data-dir> [threads] [seconds]");
    let mut cfg = CampaignConfig::new(&a[1], &a[2], std::fs::read(&a[3])?, a[4].clone().into());
    cfg.threads = a.get(5).and_then(|x| x.parse().ok()).unwrap_or(4);
    cfg.viz = std::env::var("VIZ").is_ok();
    let secs: u64 = a.get(6).and_then(|x| x.parse().ok()).unwrap_or(0);
    cfg.max_blocks = if secs == 0 { 1 } else { 0 };
    let c = Campaign::start(cfg)?;
    let events = c.subscribe();
    let t0 = std::time::Instant::now();
    let mut next_stats = std::time::Instant::now();
    let mut viz = std::collections::BTreeMap::<String, usize>::new();
    loop {
        match events.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(Event::Log(l)) => println!("{l}"),
            Ok(Event::State(s)) => println!("[state] {:?} height {} {}", s.phase, s.height, s.detail),
            Ok(Event::Viz { kind, .. }) => *viz.entry(kind).or_default() += 1,
            Ok(Event::Block(b)) => println!("[block] {b:?}"),
            Ok(Event::Stopped(e)) => {
                println!("[stopped] {e:?}; viz frames {viz:?}");
                break;
            }
            Err(_) => {}
        }
        if next_stats.elapsed().as_secs() >= 10 {
            next_stats = std::time::Instant::now();
            let s = c.stats();
            println!("[stats] {s:?} viz {viz:?}");
        }
        if secs > 0 && t0.elapsed().as_secs() >= secs {
            c.stop();
        }
    }
    c.join();
    Ok(())
}
