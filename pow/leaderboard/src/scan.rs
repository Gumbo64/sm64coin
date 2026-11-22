//! Chain scanning and persisted state.
use crate::crypto::*;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Won {
    pub height: u64,
    pub reward: u64,
    pub frames: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Participant {
    pub name: String,
    pub address: String,
    pub view_key: String,
    pub blocks: Vec<Won>,
}

/// What we keep per scanned block (coinbase only).
#[derive(Clone)]
struct CbInfo {
    hash: String,
    r: Option<EdwardsPoint>,
    outs: Vec<(u64, [u8; 32], Option<u8>)>, // amount, key, view_tag
    frames: Option<u64>,
    /// A verified on-chain opt-in tag (sm64lb) in this coinbase.
    tag: Option<sm64lb::tag::Verified>,
}

pub struct State {
    daemon: String,
    pub participants: Vec<Participant>,
    cache: Vec<CbInfo>, // index = height
}

#[derive(Serialize, Deserialize, Default)]
struct Disk {
    participants: Vec<Participant>,
}

fn rpc(daemon: &str, method: &str, params: Value) -> Result<Value, String> {
    let r: Value = ureq::post(&format!("{daemon}/json_rpc"))
        .timeout(Duration::from_secs(30))
        .send_json(json!({"jsonrpc":"2.0","id":"0","method":method,"params":params}))
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    if let Some(e) = r.get("error") {
        return Err(format!("{method}: {e}"));
    }
    Ok(r["result"].clone())
}
use std::time::Duration;

/// Extract the tx public key (tag 0x01) from raw extra bytes, walking known tags.
pub fn tx_pubkey(extra: &[u8]) -> Option<[u8; 32]> {
    let mut i = 0;
    while i < extra.len() {
        match extra[i] {
            0x00 => i += 1,
            0x01 => {
                let k = extra.get(i + 1..i + 33)?;
                return k.try_into().ok();
            }
            0x02 | 0x04 => {
                // nonce / additional pubkeys: varint length (nonce) or count*32
                let (n, l) = {
                    let mut v = 0usize;
                    let mut l = 0;
                    for (j, &b) in extra.get(i + 1..)?.iter().enumerate() {
                        v |= ((b & 0x7f) as usize) << (7 * j);
                        l = j + 1;
                        if b & 0x80 == 0 { break; }
                    }
                    (v, l)
                };
                i += 1 + l + if extra[i] == 0x02 { n } else { n * 32 };
            }
            _ => return None,
        }
    }
    None
}

fn parse_block(res: &Value) -> Result<CbInfo, String> {
    let j: Value = serde_json::from_str(res["json"].as_str().ok_or("no json")?).map_err(|e| e.to_string())?;
    let mt = &j["miner_tx"];
    let extra: Vec<u8> = mt["extra"].as_array().map(|a| a.iter().filter_map(|x| x.as_u64().map(|v| v as u8)).collect()).unwrap_or_default();
    let r = tx_pubkey(&extra).and_then(|k| CompressedEdwardsY(k).decompress());
    let mut outs = vec![];
    for o in mt["vout"].as_array().into_iter().flatten() {
        let amount = o["amount"].as_u64().unwrap_or(0);
        let t = &o["target"];
        let (k, vt) = if let Some(tk) = t.get("tagged_key") {
            (tk["key"].as_str(), tk["view_tag"].as_str().and_then(|s| u8::from_str_radix(s, 16).ok()))
        } else {
            (t["key"].as_str(), None)
        };
        let key = k.and_then(|s| hex::decode(s).ok()).and_then(|v| <[u8; 32]>::try_from(v).ok());
        outs.push((amount, key.unwrap_or([0; 32]), vt));
    }
    let outs_t: Vec<sm64lb::tag::Out> = outs.clone();
    let tag = sm64lb::tag::verify_coinbase(&extra, &outs_t);
    Ok(CbInfo {
        hash: res["block_header"]["hash"].as_str().unwrap_or("").to_string(),
        r,
        outs,
        frames: res["sm64_frames"].as_u64(),
        tag,
    })
}

fn check_block(cb: &CbInfo, height: u64, a: &Scalar, spend: &EdwardsPoint) -> Option<Won> {
    let r = cb.r.as_ref()?;
    let mut reward = 0;
    let mut hit = false;
    for (i, (amt, key, vt)) in cb.outs.iter().enumerate() {
        if output_is_ours(a, spend, r, i as u64, key, *vt) {
            reward += amt;
            hit = true;
        }
    }
    hit.then_some(Won { height, reward, frames: cb.frames })
}

fn keys_of(p: &Participant) -> Option<(Scalar, EdwardsPoint)> {
    let a = decode_address(&p.address).ok()?;
    Some((parse_view_key(&p.view_key).ok()?, CompressedEdwardsY(a.spend).decompress()?))
}

fn rescan(p: &mut Participant, cache: &[CbInfo], from: u64) {
    p.blocks.retain(|b| b.height < from);
    if let Some((a, s)) = keys_of(p) {
        for h in from as usize..cache.len() {
            if h == 0 { continue; }
            if let Some(w) = check_block(&cache[h], h as u64, &a, &s) {
                p.blocks.push(w);
            }
        }
    }
}

impl State {
    pub fn load(path: &Path, daemon: &str) -> State {
        let disk: Disk = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        State { daemon: daemon.to_string(), participants: disk.participants, cache: vec![] }
    }

    fn save(&self, path: &Path) {
        let d = Disk { participants: self.participants.clone() };
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&d).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    pub fn scanned_height(&self) -> i64 {
        self.cache.len() as i64 - 1
    }

    fn fetch(&self, h: u64) -> Result<CbInfo, String> {
        parse_block(&rpc(&self.daemon, "get_block", json!({"height": h}))?)
    }

    /// Bring the cache up to the daemon tip, rescanning from the last common height after a reorg.
    pub fn sync(&mut self) -> Result<(), String> {
        let info = rpc(&self.daemon, "get_info", json!({}))?;
        let tip = info["height"].as_u64().ok_or("no height")?; // block count
        // find last common height
        let mut keep = self.cache.len().min(tip as usize);
        while keep > 0 {
            let h = keep as u64 - 1;
            let hdr = rpc(&self.daemon, "get_block_header_by_height", json!({"height": h}))?;
            if hdr["block_header"]["hash"].as_str() == Some(self.cache[h as usize].hash.as_str()) {
                break;
            }
            keep -= 1;
        }
        if keep < self.cache.len() {
            eprintln!("reorg or rewind: rescanning from height {keep}");
        }
        self.cache.truncate(keep);
        let from = keep as u64;
        for h in from..tip {
            let cb = self.fetch(h)?;
            self.cache.push(cb);
        }
        if from < tip {
            for p in &mut self.participants {
                rescan(p, &self.cache, from);
            }
            eprintln!("scanned heights {from}..{tip}");
        }
        Ok(())
    }

    pub fn register(&mut self, name: &str, address: &str, view_key: &str, path: &Path) -> Result<Value, String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 32 || name.chars().any(|c| c.is_control()) {
            return Err("name must be 1-32 printable characters".into());
        }
        let addr = decode_address(address)?;
        let vk = parse_view_key(view_key)?;
        if !view_key_matches(&addr, &vk) {
            return Err("view key does not match the address".into());
        }
        let canon = address.trim().to_string();
        let mut p = Participant { name: name.to_string(), address: canon.clone(), view_key: hex::encode(vk.to_bytes()), blocks: vec![] };
        self.sync()?;
        rescan(&mut p, &self.cache, 0);
        let (n, total) = (p.blocks.len(), p.blocks.iter().map(|b| b.reward).sum::<u64>());
        match self.participants.iter_mut().find(|q| q.address == canon) {
            Some(q) => *q = p,
            None => self.participants.push(p),
        }
        self.save(path);
        Ok(json!({"ok": true, "blocks": n, "reward_atomic": total}))
    }

    pub fn unregister(&mut self, address: &str, view_key: &str, path: &Path) -> Result<Value, String> {
        let vk = parse_view_key(view_key)?;
        let canon = address.trim();
        let i = self.participants.iter().position(|q| q.address == canon).ok_or("address not registered")?;
        if hex::decode(&self.participants[i].view_key).ok().as_deref() != Some(&vk.to_bytes()[..]) {
            return Err("view key does not match the registration".into());
        }
        self.participants.remove(i);
        self.save(path);
        Ok(json!({"ok": true}))
    }

    pub fn leaderboard(&self) -> Value {
        use std::collections::HashMap;
        // On-chain opt-ins: verified tags in the coinbases (sm64lb), by address.
        struct Chain { name: String, blocks: u64, reward: u64, last: u64, best: Option<u64> }
        let mut chain: HashMap<String, Chain> = HashMap::new();
        for (h, cb) in self.cache.iter().enumerate() {
            if let Some(t) = &cb.tag {
                let c = chain.entry(t.address.clone()).or_insert(Chain { name: String::new(), blocks: 0, reward: 0, last: 0, best: None });
                c.name = t.name.clone();
                c.blocks += 1;
                c.reward += t.reward;
                c.last = h as u64;
                c.best = match (c.best, cb.frames) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) };
            }
        }
        // a registration and a tag are the same miner when they name the same public keys
        let canon = |a: &str| decode_address(a).ok().map(|d| (d.spend, d.view));
        let mut rows: Vec<Value> = self.participants.iter().map(|p| {
            let a = &p.address;
            let key = canon(a);
            let onchain = chain.iter().find(|(addr, _)| canon(addr) == key && key.is_some()).map(|(addr, _)| addr.clone());
            if let Some(addr) = &onchain { chain.remove(addr); }
            json!({
                "name": p.name,
                "address": a,
                "address_short": format!("{}…{}", &a[..8], &a[a.len() - 6..]),
                "blocks": p.blocks.len(),
                "reward_atomic": p.blocks.iter().map(|b| b.reward).sum::<u64>(),
                "last_block_height": p.blocks.iter().map(|b| b.height).max(),
                "best_frames": p.blocks.iter().filter_map(|b| b.frames).min(),
                "onchain": onchain.is_some(),
            })
        }).collect();
        for (a, c) in chain {
            rows.push(json!({
                "name": c.name,
                "address": a,
                "address_short": format!("{}…{}", &a[..8], &a[a.len() - 6..]),
                "blocks": c.blocks,
                "reward_atomic": c.reward,
                "last_block_height": c.last,
                "best_frames": c.best,
                "onchain": true,
            }));
        }
        rows.sort_by(|x, y| y["blocks"].as_u64().cmp(&x["blocks"].as_u64()).then(y["reward_atomic"].as_u64().cmp(&x["reward_atomic"].as_u64())));
        Value::Array(rows)
    }
}
