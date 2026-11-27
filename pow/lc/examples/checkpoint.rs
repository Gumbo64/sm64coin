//! Release tool: builds the compiled-in checkpoint table from a trusted full node.
//!
//! `cargo run --example checkpoint --features http,tls,consensus -- <node url> [--depth 36] [--out checkpoints/mainnet.json]`
//! (`scripts/update-checkpoint.sh` wraps it; run before tagging a release, see packaging/RELEASING.md).
//!
//! `... -- <node url> --check checkpoints/mainnet.json [--max-behind 2000]`: no writing; checks that the table's newest
//! checkpoint is on the node's chain and at most `max-behind` blocks below the tip (the release workflow runs it as a
//! freshness warning). Exit status 1 if the checkpoint is not on the chain, 2 if it is stale.
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
    if let Some(file) = flag("--check") {
        let max_behind: u64 = flag("--max-behind").map(|d| d.parse().expect("max-behind")).unwrap_or(2000);
        let table: Vec<Checkpoint> = serde_json::from_str(&std::fs::read_to_string(&file).expect("read the table")).expect("table parses");
        let newest = table.last().expect("non-empty table");
        let r = up.json_rpc("get_block_header_by_height", serde_json::json!({"height": newest.height}), t).await.expect("header");
        let id = r["result"]["block_header"]["hash"].as_str().unwrap_or("");
        if id != hex::encode(newest.id) {
            eprintln!("checkpoint {} ({}) is NOT on {url}'s chain (it has {id})", newest.height, hex::encode(newest.id));
            std::process::exit(1);
        }
        let behind = (height - 1).saturating_sub(newest.height);
        eprintln!("checkpoint {} is on the chain, {behind} blocks below the tip {}", newest.height, height - 1);
        if behind > max_behind {
            eprintln!("stale: more than {max_behind} blocks behind; regenerate it (scripts/update-checkpoint.sh) before the release");
            std::process::exit(2);
        }
        return;
    }
    let upto = (height - 1).saturating_sub(depth);
    // never move the anchor back (a young chain, a lagging node): keep the committed table then
    if let Some(old) = std::fs::read_to_string(&out).ok().and_then(|s| serde_json::from_str::<Vec<Checkpoint>>(&s).ok()).and_then(|t| t.last().cloned()) {
        if old.height >= upto {
            eprintln!("the table's newest checkpoint ({}) is not below the new one ({upto}): kept {out} as it is", old.height);
            return;
        }
    }
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
