//! Builds attack blocks for the denial-of-service harness (docs/DOS.md,
//! pow/tools/dos/attack.py) from real chain blocks: each output line is
//! `{"kind", "height", "id", "blob", "frames"}` (hex).
//!
//! `dos_blocks <rom> <chain.jsonl> <command> ...` where chain.jsonl has one
//! `get_block` per line (height, id, prev, ts, nonce, proof, blob,
//! miner_tx_hash):
//! - `check`: rebuilds every block's id and seed from its parts (self-test).
//! - `variants H N`: up to N valid variants of block H (its proof's last bytes
//!   changed: a different valid run, same seed and length), found by replaying.
//! - `restamps H N`: block H re-stamped N times (same run, new ids).
//! - `invalid H N FRAMES TS`: N blocks on block H's parent with other nonces
//!   (so the run no longer works), claiming FRAMES frames, stamped TS.
//! - `fakechain H N FRAMES TS`: N chained blocks on block H's parent, power-on
//!   claims of FRAMES frames each (a fake sync span).
use sha3::{Digest, Keccak256};
use sm64pow::Seed;
use sm64pow::chain::{BlockVerdict, Parent, rebuild_block_opts};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}
fn tohex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}
fn get_varint(b: &[u8], i: &mut usize) -> u64 {
    let (mut v, mut s) = (0u64, 0);
    loop {
        let x = b[*i];
        *i += 1;
        v |= ((x & 0x7f) as u64) << s;
        if x < 0x80 {
            return v;
        }
        s += 7;
    }
}
fn keccak(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Keccak256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

#[derive(Clone)]
struct Blk {
    height: u64,
    id: [u8; 32],
    prev: [u8; 32],
    ts: u64,
    proof: Vec<u8>,
    /// major, minor (the header before the timestamp).
    versions: (u64, u64),
    nonce: u32,
    /// The miner tx and tx hashes (between the header and the proof).
    middle: Vec<u8>,
    miner_tx_hash: [u8; 32],
    seed_hex: String,
}

impl Blk {
    fn parse(j: &serde_json::Value) -> Blk {
        let blob = hex(j["blob"].as_str().unwrap());
        let mut i = 0;
        let major = get_varint(&blob, &mut i);
        let minor = get_varint(&blob, &mut i);
        let ts = get_varint(&blob, &mut i);
        let prev: [u8; 32] = blob[i..i + 32].try_into().unwrap();
        i += 32;
        let nonce = u32::from_le_bytes(blob[i..i + 4].try_into().unwrap());
        i += 4;
        let proof = hex(j["proof"].as_str().unwrap());
        let mut tail = Vec::new();
        varint(proof.len() as u64, &mut tail);
        tail.extend_from_slice(&proof);
        assert!(blob.ends_with(&tail), "the proof is the blob's last field");
        let middle = blob[i..blob.len() - tail.len()].to_vec();
        Blk {
            height: j["height"].as_u64().unwrap(),
            id: hex(j["id"].as_str().unwrap()).try_into().unwrap(),
            prev,
            ts,
            proof,
            versions: (major, minor),
            nonce,
            middle,
            miner_tx_hash: hex(j["miner_tx_hash"].as_str().unwrap()).try_into().unwrap(),
            seed_hex: j["seed"].as_str().unwrap().to_string(),
        }
    }
    fn header(&self, ts: u64) -> Vec<u8> {
        let mut h = Vec::new();
        varint(self.versions.0, &mut h);
        varint(self.versions.1, &mut h);
        varint(ts, &mut h);
        h.extend_from_slice(&self.prev);
        h.extend_from_slice(&self.nonce.to_le_bytes());
        h
    }
    fn hashing_blob(&self, ts: u64) -> Vec<u8> {
        let mut h = self.header(ts);
        h.extend_from_slice(&self.miner_tx_hash); // the tree hash of one transaction is its hash
        varint(1, &mut h);
        h
    }
    fn blob(&self) -> Vec<u8> {
        let mut b = self.header(self.ts);
        b.extend_from_slice(&self.middle);
        varint(self.proof.len() as u64, &mut b);
        b.extend_from_slice(&self.proof);
        b
    }
    fn block_id(&self) -> [u8; 32] {
        // get_object_hash of a blob: cn_fast_hash(varint(size) || blob).
        let mut b = self.hashing_blob(self.ts);
        b.extend_from_slice(&sm64pow::proof_id::proof_id(&self.proof));
        let mut len = Vec::new();
        varint(b.len() as u64, &mut len);
        keccak(&[&len, &b])
    }
    fn seed(&self) -> Seed {
        Seed(keccak(&[&self.hashing_blob(0)]))
    }
    fn frames(&self) -> u32 {
        sm64pow::proof::split_blob(&self.proof).map(|x| x.0).unwrap_or(0)
    }
    fn emit(&self, kind: &str) {
        println!("{}", serde_json::json!({"kind": kind, "height": self.height, "id": tohex(&self.block_id()), "blob": tohex(&self.blob()), "frames": self.frames(), "prev": tohex(&self.prev)}));
    }
}

/// The miner tx of `b` (the start of `middle`; then a varint 0: no other txs)
/// re-addressed to `height`, and its hash: a v2 coinbase's hash is
/// H(H(prefix) || H(rct base: one 0 byte) || 32 zero bytes).
fn miner_tx_at(b: &Blk, height: u64) -> (Vec<u8>, [u8; 32]) {
    let tx = &b.middle[..b.middle.len() - 1];
    let mut i = 0;
    let version = get_varint(tx, &mut i);
    let unlock = get_varint(tx, &mut i);
    let vin = get_varint(tx, &mut i);
    assert!(version == 2 && vin == 1 && tx[i] == 0xff, "a v2 coinbase");
    i += 1;
    let old_height = get_varint(tx, &mut i);
    let mut out = Vec::new();
    varint(version, &mut out);
    varint(unlock + height - old_height, &mut out);
    varint(1, &mut out);
    out.push(0xff);
    varint(height, &mut out);
    out.extend_from_slice(&tx[i..]);
    let (prefix, base) = out.split_at(out.len() - 1);
    let hash = keccak(&[&keccak(&[prefix]), &keccak(&[base]), &[0u8; 32]]);
    out.push(0); // no other transactions
    (out, hash)
}

/// A blob claiming `frames` frames with `start`, keeping the codec and body of `like`.
fn with_header(like: &[u8], frames: u64, start: u8) -> Vec<u8> {
    let (_, _, cid, body) = sm64pow::proof::split_blob(like).unwrap();
    let mut out = Vec::new();
    varint(frames, &mut out);
    out.push(start);
    out.push(cid);
    out.extend_from_slice(body);
    out
}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    sm64pow::init(std::fs::read(&a[1])?)?;
    let chain: Vec<Blk> = std::fs::read_to_string(&a[2])?.lines().map(|l| Blk::parse(&serde_json::from_str(l).unwrap())).collect();
    let arg = |k: usize| a[k].parse::<u64>().unwrap();
    let by_height = |h: u64| chain.iter().find(|b| b.height == h).unwrap().clone();
    let parent_of = |b: &Blk| -> Parent {
        if b.height == 1 { Parent::Genesis } else { Parent::Block { id: b.prev, gap_seconds: b.ts - by_height(b.height - 1).ts } }
    };
    // Replays the chain up to block h (so variants of h can be checked against its parent's state).
    let replay_to = |h: u64| -> anyhow::Result<()> {
        for b in chain.iter().filter(|b| b.height >= 1 && b.height < h) {
            let v = rebuild_block_opts(&b.id, parent_of(b), &b.seed(), &b.proof, true)?;
            anyhow::ensure!(matches!(v, BlockVerdict::Valid { .. }), "block {} {v:?}", b.height);
        }
        Ok(())
    };
    match a[3].as_str() {
        "check" => {
            // (The genesis block is built from config, with its own id rules.)
            for b in chain.iter().filter(|b| b.height > 0) {
                assert_eq!(miner_tx_at(b, b.height), (b.middle.clone(), b.miner_tx_hash), "block {} miner tx", b.height);
                assert_eq!(b.block_id(), b.id, "block {} id", b.height);
                assert_eq!(tohex(&b.seed().0), b.seed_hex, "block {} seed", b.height);
            }
            eprintln!("{} block ids rebuilt from their parts", chain.len());
        }
        "variants" => {
            let (h, n) = (arg(4), arg(5) as usize);
            replay_to(h)?;
            let base = by_height(h);
            let seed = base.seed();
            let mut found = 0;
            let len = base.proof.len();
            let mut seen = std::collections::HashSet::new();
            'outer: for back in 1..=3usize {
                for drop in 0..3usize {
                for x in 0..=255u8 {
                    let mut p = base.proof[..len - drop].to_vec();
                    let k = p.len() - back;
                    p[k] = x;
                    if p == base.proof || !seen.insert(p.clone()) {
                        continue;
                    }
                    let v = Blk { proof: p, ..base.clone() };
                    let t0 = std::time::Instant::now();
                    let mut verdict = rebuild_block_opts(&v.block_id(), parent_of(&v), &seed, &v.proof, true)?;
                    if verdict == BlockVerdict::NeedParent {
                        // The variants' end states pushed the parent's out of the cache.
                        replay_to(h)?;
                        verdict = rebuild_block_opts(&v.block_id(), parent_of(&v), &seed, &v.proof, true)?;
                    }
                    if std::env::var("DOS_DEBUG").is_ok() && seen.len() < 30 {
                        eprintln!("back {back} drop {drop} x {x}: {:.3} s {verdict:?}", t0.elapsed().as_secs_f64());
                    }
                    if matches!(verdict, BlockVerdict::Valid { .. }) {
                        v.emit("variant");
                        found += 1;
                        if found == n {
                            break 'outer;
                        }
                    }
                }
                }
            }
            eprintln!("{found} valid variants of block {h} ({} tried)", seen.len());
        }
        "restamps" => {
            let (h, n) = (arg(4), arg(5));
            let base = by_height(h);
            for k in 1..=n {
                Blk { ts: base.ts + k, ..base.clone() }.emit("restamp");
            }
        }
        "invalid" => {
            let (h, n, frames, ts) = (arg(4), arg(5), arg(6), arg(7));
            let base = by_height(h);
            let proof = with_header(&base.proof, frames, 0);
            for k in 1..=n {
                Blk { nonce: base.nonce.wrapping_add(k as u32), ts, proof: proof.clone(), ..base.clone() }.emit("invalid");
            }
        }
        "fakechain" => {
            let (h, n, frames, ts) = (arg(4), arg(5), arg(6), arg(7));
            let base = by_height(h);
            let proof = with_header(&base.proof, frames, 1);
            let mut prev = base.prev;
            for k in 0..n {
                let (middle, miner_tx_hash) = miner_tx_at(&base, h + k);
                let b = Blk { prev, height: h + k, ts: ts + k, nonce: base.nonce.wrapping_add(1000 + k as u32), proof: proof.clone(), middle, miner_tx_hash, ..base.clone() };
                b.emit("fake");
                prev = b.block_id();
            }
        }
        x => anyhow::bail!("unknown command {x}"),
    }
    Ok(())
}
