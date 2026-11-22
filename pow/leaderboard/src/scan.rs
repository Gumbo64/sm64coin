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
    /// "local" (registered here) or "peer" (learned from another leaderboard server).
    #[serde(default = "local")]
    pub source: String,
    #[serde(default)]
    pub learned_from: Option<String>,
    /// Unix seconds when first added.
    #[serde(default)]
    pub added: u64,
}

fn local() -> String {
    "local".into()
}

/// Most entries a server tracks; peer-learned ones are evicted (lowest score first) to stay under it.
pub const MAX_TRACKED: usize = 1000;
/// Size of the shared / displayed board.
pub const TOP_N: usize = 50;
/// Most entries taken from one peer response.
pub const MAX_PER_PULL: usize = 100;

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Default, Debug, PartialEq)]
pub struct MergeReport {
    pub added: usize,
    pub rejected: usize,
    pub known: usize,
}

/// What we keep per scanned block (coinbase only).
#[derive(Clone)]
struct CbInfo {
    hash: String,
    r: Option<EdwardsPoint>,
    outs: Vec<(u64, [u8; 32], Option<u8>)>, // amount, key, view_tag
    frames: Option<u64>,
}

pub struct State {
    daemon: String,
    pub participants: Vec<Participant>,
    cache: Vec<CbInfo>, // index = height
    synced: bool,
    /// Addresses removed here with /unregister: never re-learned from peers.
    blocked: Vec<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct Disk {
    participants: Vec<Participant>,
    #[serde(default)]
    blocked: Vec<String>,
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
    Ok(CbInfo {
        hash: res["block_header"]["hash"].as_str().unwrap_or("").to_string(),
        r,
        outs,
        frames: res["sm64_frames"].as_u64(),
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
        // Entries on disk were verified when added; drop any that no longer pass.
        let participants = disk.participants.into_iter().filter(|p| validate(&p.name, &p.address, &p.view_key).is_ok()).collect();
        State { daemon: daemon.to_string(), participants, cache: vec![], synced: false, blocked: disk.blocked }
    }

    fn save(&self, path: &Path) {
        let d = Disk { participants: self.participants.clone(), blocked: self.blocked.clone() };
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
        self.synced = true;
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
        let mut p = validate(name, address, view_key)?;
        self.sync()?;
        rescan(&mut p, &self.cache, 0);
        let (n, total) = (p.blocks.len(), p.blocks.iter().map(|b| b.reward).sum::<u64>());
        self.blocked.retain(|a| *a != p.address);
        match self.participants.iter_mut().find(|q| q.address == p.address) {
            Some(q) => *q = p,
            None => self.participants.push(p),
        }
        self.save(path);
        Ok(json!({"ok": true, "blocks": n, "reward_atomic": total}))
    }

    /// Add entries learned from a peer. Each is verified here (the view key must match the address and
    /// the blocks are counted by our own chain scan); stats sent by the peer are never used.
    pub fn merge_peer(&mut self, entries: &[Value], from: &str, path: &Path) -> MergeReport {
        let mut rep = MergeReport::default();
        for e in entries.iter().take(MAX_PER_PULL) {
            let f = |k: &str| e.get(k).and_then(|v| v.as_str()).unwrap_or("");
            let Ok(mut p) = validate(f("name"), f("address"), f("view_key")) else {
                rep.rejected += 1;
                continue;
            };
            if self.blocked.contains(&p.address) {
                rep.rejected += 1;
                continue;
            }
            if self.participants.iter().any(|q| q.address == p.address) {
                rep.known += 1; // same address means same view key (it is determined by the address); local entry or first-seen name wins
                continue;
            }
            p.source = "peer".into();
            p.learned_from = Some(from.to_string());
            rescan(&mut p, &self.cache, 0);
            if self.participants.len() >= MAX_TRACKED {
                let worst = self.participants.iter().enumerate().filter(|(_, q)| q.source != "local").min_by_key(|(_, q)| score(q)).map(|(i, q)| (i, score(q)));
                match worst {
                    Some((i, w)) if score(&p) > w => {
                        self.participants.remove(i);
                    }
                    _ => {
                        rep.rejected += 1;
                        continue;
                    }
                }
            }
            self.participants.push(p);
            rep.added += 1;
        }
        if rep.added > 0 {
            self.save(path);
        }
        rep
    }

    pub fn peer_entries(&self) -> usize {
        self.participants.iter().filter(|p| p.source != "local").count()
    }

    /// What /share returns: the top entries with everything a peer needs to verify them itself.
    /// (blocks/reward are for display only; receivers recompute them.)
    pub fn share(&self) -> Vec<Value> {
        if !self.synced {
            return vec![];
        }
        self.top().into_iter().map(|p| json!({
            "name": p.name, "address": p.address, "view_key": p.view_key,
            "blocks": p.blocks.len(), "reward_atomic": reward(p), "best_frames": p.blocks.iter().filter_map(|b| b.frames).min(),
        })).collect()
    }

    fn top(&self) -> Vec<&Participant> {
        let mut v: Vec<&Participant> = self.participants.iter().collect();
        v.sort_by(|x, y| score(y).cmp(&score(x)).then(x.address.cmp(&y.address)));
        v.truncate(TOP_N);
        v
    }

    pub fn unregister(&mut self, address: &str, view_key: &str, path: &Path) -> Result<Value, String> {
        let vk = parse_view_key(view_key)?;
        let canon = address.trim();
        let i = self.participants.iter().position(|q| q.address == canon).ok_or("address not registered")?;
        if hex::decode(&self.participants[i].view_key).ok().as_deref() != Some(&vk.to_bytes()[..]) {
            return Err("view key does not match the registration".into());
        }
        self.participants.remove(i);
        if !self.blocked.iter().any(|a| a == canon) && self.blocked.len() < 10_000 {
            self.blocked.push(canon.to_string());
        }
        self.save(path);
        Ok(json!({"ok": true}))
    }

    /// The board: top entries by blocks, then reward. Every entry was verified by this server's own scan.
    pub fn leaderboard(&self) -> Value {
        let rows: Vec<Value> = self.top().into_iter().map(|p| {
            let a = &p.address;
            json!({
                "name": p.name,
                "address": a,
                "address_short": format!("{}…{}", &a[..8], &a[a.len() - 6..]),
                "blocks": p.blocks.len(),
                "reward_atomic": reward(p),
                "last_block_height": p.blocks.iter().map(|b| b.height).max(),
                "best_frames": p.blocks.iter().filter_map(|b| b.frames).min(),
                "verified": self.synced,
                "source": p.source,
            })
        }).collect();
        Value::Array(rows)
    }
}

fn reward(p: &Participant) -> u64 {
    p.blocks.iter().map(|b| b.reward).sum()
}

fn score(p: &Participant) -> (usize, u64) {
    (p.blocks.len(), reward(p))
}

/// Check a name / address / view key triple: well formed, and the private view key belongs to the address.
pub fn validate(name: &str, address: &str, view_key: &str) -> Result<Participant, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 32 || name.chars().any(|c| c.is_control()) {
        return Err("name must be 1-32 printable characters".into());
    }
    if address.len() > 200 || view_key.len() > 80 {
        return Err("field too long".into());
    }
    let addr = decode_address(address)?;
    let vk = parse_view_key(view_key)?;
    if !view_key_matches(&addr, &vk) {
        return Err("view key does not match the address".into());
    }
    Ok(Participant { name: name.to_string(), address: address.trim().to_string(), view_key: hex::encode(vk.to_bytes()), blocks: vec![], source: "local".into(), learned_from: None, added: now() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use curve25519_dalek::constants::ED25519_BASEPOINT_POINT as G;

    struct Keys {
        addr: String,
        vk: String,
        a: Scalar,
        b: Scalar,
    }

    fn keys(seed: u8) -> Keys {
        let a = Scalar::from_bytes_mod_order([seed; 32]);
        let b = Scalar::from_bytes_mod_order([seed.wrapping_add(100); 32]);
        let addr = encode_address(&Address { prefix: PREFIX_STANDARD, spend: (b * G).compress().to_bytes(), view: (a * G).compress().to_bytes() });
        Keys { addr, vk: hex::encode(a.to_bytes()), a, b }
    }

    /// A coinbase paying `k` (one output), the way the node builds it.
    fn coinbase(k: &Keys, rseed: u8, reward: u64) -> CbInfo {
        let r = Scalar::from_bytes_mod_order([rseed; 32]);
        let rp = r * G;
        let d = (r * (k.a * G)).mul_by_cofactor().compress().to_bytes();
        let mut m = d.to_vec();
        m.extend(varint(0));
        let key = (hash_to_scalar(&m) * G + k.b * G).compress().to_bytes();
        CbInfo { hash: format!("h{rseed}"), r: Some(rp), outs: vec![(reward, key, None)], frames: Some(100) }
    }

    fn state(chain: Vec<CbInfo>) -> State {
        State { daemon: "http://127.0.0.1:1".into(), participants: vec![], cache: chain, synced: true, blocked: vec![] }
    }

    fn entry(k: &Keys, name: &str) -> Value {
        json!({"name": name, "address": k.addr, "view_key": k.vk, "blocks": 9999, "reward_atomic": 9999})
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lbtest-{}-{name}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d.join("lb.json")
    }

    fn dummy_chain(k1: &Keys, k2: &Keys) -> Vec<CbInfo> {
        // height 0 unused, 1..=3 -> k1, 4 -> k2
        let mut c = vec![coinbase(k1, 200, 1)];
        c.extend([coinbase(k1, 1, 10), coinbase(k1, 2, 10), coinbase(k1, 3, 10), coinbase(k2, 4, 7)]);
        c
    }

    #[test]
    fn merge_verifies_with_own_scan_and_ignores_peer_stats() {
        let (k1, k2) = (keys(7), keys(9));
        let mut st = state(dummy_chain(&k1, &k2));
        let p = tmp("merge");
        let rep = st.merge_peer(&[entry(&k1, "one"), entry(&k2, "two")], "http://peer", &p);
        assert_eq!(rep, MergeReport { added: 2, rejected: 0, known: 0 });
        let board = st.leaderboard();
        assert_eq!(board[0]["address"], k1.addr);
        assert_eq!(board[0]["blocks"], 3); // our scan, not the claimed 9999
        assert_eq!(board[1]["blocks"], 1);
        assert_eq!(board[0]["source"], "peer");
        // persisted and reloadable
        let again = State::load(&p, "x");
        assert_eq!(again.participants.len(), 2);
    }

    #[test]
    fn forged_entries_rejected() {
        let (k1, k2) = (keys(7), keys(9));
        let mut st = state(dummy_chain(&k1, &k2));
        let p = tmp("forged");
        // k2's address with k1's view key; garbage; bad name; non-hex key
        let bad = vec![
            json!({"name": "x", "address": k2.addr, "view_key": k1.vk}),
            json!({"name": "x", "address": "SM64nonsense", "view_key": k1.vk}),
            json!({"name": "", "address": k1.addr, "view_key": k1.vk}),
            json!({"name": "x", "address": k1.addr, "view_key": "zz"}),
            json!({"address": k1.addr}),
            json!(5),
        ];
        let rep = st.merge_peer(&bad, "http://peer", &p);
        assert_eq!((rep.added, rep.rejected), (0, 6));
        assert!(st.participants.is_empty());
    }

    #[test]
    fn local_entry_wins_conflicts_and_blocked_stay_out() {
        let (k1, k2) = (keys(7), keys(9));
        let mut st = state(dummy_chain(&k1, &k2));
        let p = tmp("conflict");
        let mut mine = validate("mine", &k1.addr, &k1.vk).unwrap();
        rescan(&mut mine, &st.cache, 0);
        st.participants.push(mine);
        let rep = st.merge_peer(&[entry(&k1, "renamed"), json!({"name": "y", "address": k1.addr, "view_key": k2.vk})], "http://peer", &p);
        assert_eq!((rep.added, rep.known, rep.rejected), (0, 1, 1));
        assert_eq!(st.participants[0].name, "mine");
        assert_eq!(st.participants[0].source, "local");
        // unregistering blocks re-learning
        st.unregister(&k1.addr, &k1.vk, &p).unwrap();
        let rep = st.merge_peer(&[entry(&k1, "mine")], "http://peer", &p);
        assert_eq!((rep.added, rep.rejected), (0, 1));
        assert_eq!(State::load(&p, "x").blocked.len(), 1);
    }

    #[test]
    fn caps_and_eviction() {
        let (k1, k2) = (keys(7), keys(9));
        let mut st = state(dummy_chain(&k1, &k2));
        let p = tmp("caps");
        // more than MAX_PER_PULL entries in one response: only the first 100 are considered
        let many: Vec<Value> = (0..150u32).map(|i| { let k = keys((i % 200) as u8 + 20); json!({"name": format!("n{i}"), "address": k.addr, "view_key": k.vk}) }).collect();
        let rep = st.merge_peer(&many, "http://peer", &p);
        assert_eq!(rep.added + rep.known, MAX_PER_PULL);
        // fill to the cap with zero-score peer entries, then a better entry evicts the worst, a worse one is dropped
        let template = st.participants[0].clone();
        st.participants.clear();
        for i in 0..MAX_TRACKED {
            let mut q = template.clone();
            q.address = format!("fake{i}");
            q.source = "peer".into();
            st.participants.push(q);
        }
        assert_eq!(st.merge_peer(&[entry(&keys(7), "one")], "http://peer", &p).added, 1);
        assert_eq!(st.participants.len(), MAX_TRACKED);
        assert!(st.participants.iter().any(|q| q.address == k1.addr));
        assert_eq!(st.merge_peer(&[entry(&keys(8), "zero")], "http://peer", &p).rejected, 1); // no blocks: not better than the worst
        // local entries are never evicted
        for q in st.participants.iter_mut() { q.source = "local".into(); }
        assert_eq!(st.merge_peer(&[entry(&k2, "two")], "http://peer", &p).rejected, 1);
    }

    #[test]
    fn share_format_top_n_and_unsynced() {
        let (k1, k2) = (keys(7), keys(9));
        let mut st = state(dummy_chain(&k1, &k2));
        let p = tmp("share");
        let mut es = vec![entry(&k1, "one"), entry(&k2, "two")];
        es.extend((0..60u8).map(|i| { let k = keys(30 + i); entry(&k, &format!("z{i}")) }));
        st.merge_peer(&es, "http://peer", &p);
        let sh = st.share();
        assert_eq!(sh.len(), TOP_N);
        assert_eq!(sh[0]["address"], k1.addr);
        for k in ["name", "address", "view_key", "blocks", "reward_atomic", "best_frames"] {
            assert!(sh[0].get(k).is_some(), "missing {k}");
        }
        assert_eq!(sh[0]["view_key"], k1.vk);
        assert_eq!(st.leaderboard().as_array().unwrap().len(), TOP_N);
        st.synced = false;
        assert!(st.share().is_empty());
    }
}
