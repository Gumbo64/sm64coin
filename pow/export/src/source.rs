//! Where blocks come from: a JSONL dump (the test fixtures' format) or a
//! daemon's RPC (`get_block`). Read-only: nothing is ever submitted.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

/// A block as the exporter needs it.
#[derive(Clone, Debug)]
pub struct BlockRec {
    pub height: u64,
    pub id: [u8; 32],
    pub prev: [u8; 32],
    pub ts: u64,
    /// `sm64_seed_hash`: the perturbation seed.
    pub seed: [u8; 32],
    pub proof: Vec<u8>,
    /// v0.3: the miner's key (from the coinbase extra nonce) and the block's signature.
    pub miner_key: Option<[u8; 32]>,
    pub sig: Option<[u8; 64]>,
    pub difficulty: Option<u64>,
}

fn h32(s: &str) -> Result<[u8; 32]> {
    hex::decode(s)?.try_into().map_err(|_| anyhow!("expected 32 bytes of hex, got {s:?}"))
}

/// The coinbase's extra nonce (tx_extra tag 0x02) from the `extra` byte array.
pub fn extra_nonce(extra: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0;
    while i < extra.len() {
        match extra[i] {
            0x00 => i += 1,                    // padding
            0x01 => i += 33,                   // tx public key
            0x02 => {                          // nonce: varint length, bytes
                let (mut len, mut shift) = (0usize, 0);
                i += 1;
                loop {
                    let b = *extra.get(i)?;
                    i += 1;
                    len |= ((b & 0x7f) as usize) << shift;
                    shift += 7;
                    if b & 0x80 == 0 || shift > 28 {
                        break;
                    }
                }
                let body = extra.get(i..i + len)?;
                if body.first() == Some(&sm64pow::sign::KEY_TAG) {
                    return Some(body.to_vec());
                }
                i += len;
            }
            _ => return None,
        }
    }
    None
}

/// One JSONL line: `{height, id, prev, ts, seed, proof[, difficulty, miner_key, sig]}` (hex strings).
pub fn parse_line(l: &str) -> Result<BlockRec> {
    let j: Value = serde_json::from_str(l)?;
    let s = |k: &str| j[k].as_str().ok_or_else(|| anyhow!("missing field {k}"));
    Ok(BlockRec {
        height: j["height"].as_u64().ok_or_else(|| anyhow!("missing height"))?,
        id: h32(s("id")?)?,
        prev: h32(s("prev")?)?,
        ts: j["ts"].as_u64().ok_or_else(|| anyhow!("missing ts"))?,
        seed: h32(s("seed")?)?,
        proof: hex::decode(s("proof")?)?,
        miner_key: j["miner_key"].as_str().map(h32).transpose()?,
        sig: j["sig"].as_str().map(|x| hex::decode(x)?.try_into().map_err(|_| anyhow!("sig is not 64 bytes"))).transpose()?,
        difficulty: j["difficulty"].as_u64(),
    })
}

pub fn read_jsonl(text: &str) -> Result<Vec<BlockRec>> {
    let mut v = Vec::new();
    for (n, l) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        v.push(parse_line(l).with_context(|| format!("line {}", n + 1))?);
    }
    v.sort_by_key(|b| b.height);
    Ok(v)
}

/// The JSONL line of `b` (the inverse of [`parse_line`]).
pub fn to_line(b: &BlockRec) -> String {
    let mut j = json!({"height": b.height, "id": hex::encode(b.id), "prev": hex::encode(b.prev), "ts": b.ts,
        "seed": hex::encode(b.seed), "proof": hex::encode(&b.proof)});
    if let Some(d) = b.difficulty { j["difficulty"] = d.into(); }
    if let Some(k) = b.miner_key { j["miner_key"] = hex::encode(k).into(); }
    if let Some(s) = b.sig { j["sig"] = hex::encode(s).into(); }
    j.to_string()
}

pub struct Rpc {
    url: String,
    agent: ureq::Agent,
}

impl Rpc {
    pub fn new(url: &str) -> Rpc {
        Rpc { url: url.trim_end_matches('/').to_string(), agent: ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(120)).build() }
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 0, "method": method, "params": params});
        let resp: Value = self.agent.post(&format!("{}/json_rpc", self.url)).send_json(body)?.into_json()?;
        if let Some(e) = resp.get("error") {
            bail!("{method}: {}", e.get("message").and_then(|m| m.as_str()).unwrap_or("error"));
        }
        resp.get("result").cloned().ok_or_else(|| anyhow!("{method}: no result"))
    }

    /// The chain's height (the next block's height).
    pub fn height(&self) -> Result<u64> {
        self.call("get_info", json!({}))?["height"].as_u64().ok_or_else(|| anyhow!("get_info: no height"))
    }

    pub fn block(&self, height: u64) -> Result<BlockRec> {
        let r = self.call("get_block", json!({"height": height}))?;
        parse_block(&r).with_context(|| format!("block {height}"))
    }
}

/// A `get_block` result. v0.2 carries `sm64_proof` in the block's JSON; v0.3
/// also `sm64_sig` (the last 64 bytes of the block blob) and the miner key in
/// the coinbase extra nonce.
pub fn parse_block(r: &Value) -> Result<BlockRec> {
    let hd = &r["block_header"];
    let body: Value = serde_json::from_str(r["json"].as_str().ok_or_else(|| anyhow!("no json"))?)?;
    let hexf = |v: &Value| v.as_str().map(hex::decode).transpose();
    let blob = hexf(&r["blob"])?;
    let proof_len = r["sm64_proof_bytes"].as_u64().map(|x| x as usize);
    let sig: Option<[u8; 64]> = match hexf(&body["sm64_sig"])? {
        Some(s) => Some(s.try_into().map_err(|_| anyhow!("sm64_sig is not 64 bytes"))?),
        None => None,
    };
    let proof = match hexf(&body["sm64_proof"])? {
        Some(p) => p,
        None => {
            // The block blob ends proof || sig (docs/V0.3-NEW-CHAIN.md).
            let (b, n) = (blob.as_ref().ok_or_else(|| anyhow!("no blob"))?, proof_len.ok_or_else(|| anyhow!("no sm64_proof_bytes"))?);
            let end = b.len().checked_sub(64).ok_or_else(|| anyhow!("blob too short"))?;
            b[end - n..end].to_vec()
        }
    };
    let sig = sig.or_else(|| blob.as_ref().filter(|_| body["sm64_proof"].is_null()).map(|b| b[b.len() - 64..].try_into().unwrap()));
    let extra: Vec<u8> = body["miner_tx"]["extra"].as_array().map(|a| a.iter().filter_map(|x| x.as_u64().map(|x| x as u8)).collect()).unwrap_or_default();
    let miner_key = extra_nonce(&extra).and_then(|n| sm64pow::sign::miner_key(&n));
    Ok(BlockRec {
        height: hd["height"].as_u64().ok_or_else(|| anyhow!("no height"))?,
        id: h32(hd["hash"].as_str().ok_or_else(|| anyhow!("no hash"))?)?,
        prev: h32(hd["prev_hash"].as_str().ok_or_else(|| anyhow!("no prev_hash"))?)?,
        ts: hd["timestamp"].as_u64().ok_or_else(|| anyhow!("no timestamp"))?,
        seed: h32(r["sm64_seed_hash"].as_str().ok_or_else(|| anyhow!("no sm64_seed_hash"))?)?,
        proof,
        miner_key,
        sig,
        difficulty: hd["difficulty"].as_u64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_lines_parse_and_round_trip() {
        let t = include_str!("../../sm64pow/tests/fixtures/mainnet_v02.jsonl");
        let v = read_jsonl(t).unwrap();
        assert_eq!(v.len(), 34);
        assert_eq!(v[0].height, 1);
        let again = read_jsonl(&v.iter().map(to_line).collect::<Vec<_>>().join("\n")).unwrap();
        assert_eq!(again[5].proof, v[5].proof);
        assert_eq!(again[5].id, v[5].id);
    }

    #[test]
    fn extra_nonce_parsing() {
        let mut e = vec![1u8];
        e.extend([7u8; 32]);
        assert_eq!(extra_nonce(&e), None, "v0.2: a public key only");
        let mut key = vec![sm64pow::sign::KEY_TAG];
        key.extend([9u8; 32]);
        e.push(2);
        e.push(33);
        e.extend(&key);
        assert_eq!(extra_nonce(&e), Some(key));
    }

    /// A v0.3 `get_block`: the proof and signature come from the block blob, the miner key from the coinbase.
    #[test]
    fn parse_a_v03_block() {
        let key = sm64pow::sign::Keypair::from_entropy(&[7u8; 32]).unwrap();
        let mut extra = vec![1u8];
        extra.extend([3u8; 32]);
        extra.push(2);
        extra.push(33);
        extra.extend(key.extra_nonce());
        let proof = vec![5u8, 1, 0, 9, 9, 9];
        let mut blob = vec![0xAAu8; 40];
        blob.extend(&proof);
        blob.extend([0xEEu8; 64]);
        let r = json!({
            "blob": hex::encode(&blob), "sm64_proof_bytes": proof.len(), "sm64_seed_hash": "11".repeat(32),
            "block_header": {"height": 7, "hash": "22".repeat(32), "prev_hash": "33".repeat(32), "timestamp": 99, "difficulty": 3},
            "json": json!({"miner_tx": {"extra": extra}}).to_string(),
        });
        let b = parse_block(&r).unwrap();
        assert_eq!((b.height, b.ts, b.proof.clone(), b.sig), (7, 99, proof, Some([0xEE; 64])));
        assert_eq!(b.miner_key, Some(key.public));
    }
}
