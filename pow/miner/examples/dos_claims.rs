//! Block claims for the DoS harness (pow/tools/dos/attack.py, docs/DOS.md), v0.3:
//! signed blocks on the victim's tip whose runs are junk (all-zero inputs of the
//! claimed length: the replay runs to the end and fails), and honest blocks from
//! `automine --save-block` files. One JSON line per block: {blob, id, height, frames, key}.
//!
//!   dos_claims DAEMON_RPC fake  N FRAMES [KEYS]  fresh key per claim (KEYS=0), or KEYS keys round robin
//!   dos_claims DAEMON_RPC samekey N FRAMES       one key, N different runs of its single template
//!   dos_claims DAEMON_RPC honest SAVED.json      the saved miner's block, stamped now
//!
//! Each claim uses its own template (get_block_template with its own key's extra nonce).
use anyhow::Result;
use serde_json::{Value, json};
use sm64coin_miner::rpc::{Daemon, seal_with};
use sm64pow::proof::{Proof, Start, encode_proof};
use sm64pow::sign::Keypair;
use sm64_binds::GamePad;

const MAINNET_ADDR: &str = "SM64RtPqsHM63LZn2u9Myx9HtC4kjhu1h98SRtJz9USB7NZAoCMdZSBD7D1L2DWu9uCYZFUSVKytreuzEMJ6VjBU2tbjKqevFH";

/// The reward address for templates: $SM64_ADDR (a test network's) or a mainnet one.
fn addr() -> String {
    std::env::var("SM64_ADDR").unwrap_or_else(|_| MAINNET_ADDR.into())
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn junk(frames: u32, variant: u32, start: Start) -> Vec<u8> {
    let mut inputs = vec![GamePad::default(); frames as usize];
    if let Some(p) = inputs.last_mut() {
        p.button = (variant & 0xffff) as u16; // variants differ in their last input
    }
    encode_proof(&Proof { frames, start, inputs })
}

fn line(blob: &str, height: u64, frames: u32, key: &Keypair) -> Value {
    json!({"blob": blob, "id": "", "height": height, "frames": frames, "key": hex::encode(key.public)})
}

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let d = Daemon::new(&a[1]);
    let height = d.height()?; // the next block's height
    let top = d.call("get_block_template", json!({"wallet_address": addr()}))?;
    let start = match top["sm64_start"].as_str().unwrap_or("parent") {
        "power-on" => Start::PowerOn,
        _ => Start::Parent,
    };
    match a[2].as_str() {
        "honest" => {
            let s: Value = serde_json::from_str(&std::fs::read_to_string(&a[3])?)?;
            let g = |k: &str| s[k].as_str().unwrap_or_default().to_string();
            let key = Keypair::from_secret(&<[u8; 32]>::try_from(hex::decode(g("key_secret"))?.as_slice())?).unwrap();
            let proof = hex::decode(g("proof"))?;
            let frames = s["frames"].as_u64().unwrap() as u32;
            println!("{}", line(&seal_with(&g("template_blob"), &g("seed_hash"), &key, &proof, Some(now()))?, height, frames, &key));
        }
        mode @ ("fake" | "samekey") => {
            let n: u32 = a[3].parse()?;
            let frames: u32 = a[4].parse()?;
            let keys: usize = a.get(5).map_or(0, |k| k.parse().unwrap_or(0));
            let pool: Vec<Keypair> = (0..keys).map(|_| Keypair::generate()).collect();
            let one = Keypair::generate();
            // samekey: one template, N different signed runs.
            let one_t = if mode == "samekey" { Some(d.template_for(&addr(), one.clone())?) } else { None };
            for i in 0..n {
                let (t, key) = match &one_t {
                    Some(t) => (t.clone(), one.clone()),
                    None => {
                        let k = if keys > 0 { pool[i as usize % keys].clone() } else { Keypair::generate() };
                        // A key reused for several templates would be equivocation: its claims share a template only if the daemon returns the same one (same extra nonce): fine, they are then different runs of one seed.
                        (d.template_for(&addr(), k.clone())?, k)
                    }
                };
                let proof = junk(frames, i + 1, start);
                println!("{}", line(&seal_with(&t.blocktemplate_blob, &t.sm64_seed_hash, &key, &proof, Some(now()))?, height - 1, frames, &key));
            }
        }
        _ => anyhow::bail!("fake | samekey | honest"),
    }
    Ok(())
}
