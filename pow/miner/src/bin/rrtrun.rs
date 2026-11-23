//! One tree step of the RRT miner (rrt/child.rs), as its own process:
//! `rrtrun <job.json> <out.json>`.
use anyhow::Result;

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().collect();
    anyhow::ensure!(a.len() == 3, "usage: rrtrun <job.json> <out.json>");
    // Die with the miner (a killed automine must not leave a tree running).
    #[cfg(unix)]
    {
        let parent = std::os::unix::process::parent_id();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if std::os::unix::process::parent_id() != parent {
                std::process::exit(1);
            }
        });
    }
    let job: sm64coin_miner::rrt::child::Job = serde_json::from_str(&std::fs::read_to_string(&a[1])?)?;
    let out = sm64coin_miner::rrt::child::run_step(&job)?;
    std::fs::write(&a[2], serde_json::to_string(&out)?)?;
    Ok(())
}
