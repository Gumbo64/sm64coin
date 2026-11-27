//! One tree step of the RRT miner (rrt/child.rs), as its own process:
//! `rrtrun <job.json> <out.json>`. With `--inproc` first: the same step through the in-process runner (a thread with its own
//! config, run-time state and rayon pool; rrt/child.rs `run_step_inproc`), which the gate compares with the process mode.
use anyhow::Result;

fn main() -> Result<()> {
    let mut a: Vec<String> = std::env::args().collect();
    let inproc = a.get(1).is_some_and(|x| x == "--inproc");
    if inproc {
        a.remove(1);
    }
    anyhow::ensure!(a.len() == 3, "usage: rrtrun [--inproc] <job.json> <out.json>");
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
    if job.viz {
        // The desktop app's feed (automine --viz): stdin/stdout carry it (rrt/vizio.rs).
        sm64coin_miner::rrt::vizio::child_start(format!("{}.progress", job.out_dir));
    }
    let out = if inproc {
        sm64pow::init(std::fs::read(&job.rom)?)?;
        let job2 = job.clone();
        std::thread::Builder::new().stack_size(32 << 20).spawn(move || sm64coin_miner::rrt::child::run_step_inproc(&job2, &Default::default()))?.join().map_err(|_| anyhow::anyhow!("step panicked"))??
    } else {
        sm64coin_miner::rrt::child::run_step(&job)?
    };
    std::fs::write(&a[2], serde_json::to_string(&out)?)?;
    Ok(())
}
