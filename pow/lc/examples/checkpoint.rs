//! Release tool: builds the compiled-in checkpoint table from a trusted full node.
//!
//! `cargo run --example checkpoint --features http,tls,consensus -- <node url> [--depth 36] [--out checkpoints/mainnet.json]`
//!
//! Fetches every block from genesis, runs the verifier's own header rules over them (so the
//! recorded weight and difficulty window are exactly what a client will recompute) and writes the
//! genesis checkpoint plus one `depth` blocks below the tip (36 = the node's reorg cap). Needs a
//! node you trust; run it on a machine you control, against your own node.
use std::time::Duration;

use sm64coin_lc::block::RpcBlock;
use sm64coin_lc::consensus::Sm64Consensus;
use sm64coin_lc::exec::Upstream;
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::verify::Checkpoint;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let url = args.first().expect("usage: checkpoint <node url> [--depth N] [--out FILE]");
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned());
    let depth: u64 = flag("--depth").map(|d| d.parse().expect("depth")).unwrap_or(36);
    let out = flag("--out").unwrap_or_else(|| "checkpoints/mainnet.json".into());
    let up = HttpUpstream::new(url).expect("node url");
    let t = Duration::from_secs(30);
    let info = up.json_rpc("get_info", serde_json::json!({}), t).await.expect("get_info");
    let height = info["result"]["height"].as_u64().expect("height");
    let upto = (height - 1).saturating_sub(depth);
    let mut blocks = Vec::new();
    for h in 0..=upto {
        let r = up.json_rpc("get_block", serde_json::json!({"height": h}), t).await.expect("get_block");
        blocks.push(RpcBlock::from_json(&r["result"].to_string()).expect("block"));
    }
    let mut table = vec![Checkpoint::genesis(blocks[0].claims.hash)];
    if upto > 0 {
        table.push(Checkpoint::build(&blocks, upto, &Sm64Consensus).expect("the node's chain verifies"));
    }
    std::fs::write(&out, serde_json::to_string_pretty(&table).unwrap() + "\n").expect("write");
    eprintln!("wrote {} checkpoint(s) to {out}, newest at height {upto}", table.len());
}
