//! Compares the node's end state (get_sm64_state) with the cycle replay for a tip.
//! Usage: state_eq <daemon url> <rom> [tip hash]   (default: the chain's top)
use sm64coin_miner::rpc;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (url, rom) = (&args[1], &args[2]);
    sm64pow::init(std::fs::read(rom)?)?;
    let d = rpc::Daemon::new(url);
    let tip = match args.get(3) {
        Some(h) => h.clone(),
        None => d.top_hash()?,
    };
    let node = d.end_state(&tip)?;
    let (replay, p) = rpc::cycle_end_state(&d.cycle(Some(&tip))?)?;
    let np = rpc::state_progress(&node)?;
    println!("tip {tip}\nreplay progress {p:?}\nnode   progress {np:?}");
    let (a, b) = (node.as_bytes(), replay.as_bytes());
    println!("state bytes: node {} replay {} equal {}", a.len(), b.len(), a == b);
    let region = |s: &sm64_binds::SaveState| -> anyhow::Result<Vec<u8>> {
        let mut g = sm64pow::generator()?.create_game()?;
        g.load_state(s)?;
        sm64pow::game::read_region_bytes(&g)
    };
    let (ra, rb) = (region(&node)?, region(&replay)?);
    println!("perturbation region bytes: {} equal {}", ra.len(), ra == rb);
    anyhow::ensure!(np.stars == p.stars && np.keys == p.keys && ra == rb, "MISMATCH");
    Ok(())
}
