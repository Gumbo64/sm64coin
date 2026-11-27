//! The scored node list: candidates from every source, scoring, weighted-random choice among the
//! best, bans, persistence. Pure logic (no I/O, no clock, no RNG: the caller passes `now` in unix
//! seconds and a random `u64`), so the browser and the apps run the same code.
use serde::{Deserialize, Serialize};

use crate::proto::MAX_NODES;

/// A node is skipped when it is more than this many blocks behind the best tip seen.
pub const MAX_HEIGHT_LAG: u64 = 2;
/// Ban for a node caught lying (a week).
pub const LIAR_BAN_SECS: u64 = 7 * 24 * 3600;
/// Selection is weighted-random among this many best nodes.
pub const PICK_TOP: usize = 5;
const EWMA_ALPHA: f64 = 0.2;

/// How to reach a node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeAddr {
    /// A JSON-RPC node (`https://host[:port]`, or plain http for list members).
    Http { url: String },
    /// An iroh endpoint: id (hex), optional home relay, optional direct "ip:port" addresses.
    Iroh { endpoint_id: String, relay_url: Option<String>, direct: Vec<String> },
}

impl NodeAddr {
    /// Stable identity: the URL or the endpoint id (addresses may change).
    pub fn key(&self) -> String {
        match self {
            NodeAddr::Http { url } => format!("http:{}", url.trim_end_matches('/')),
            NodeAddr::Iroh { endpoint_id, .. } => format!("iroh:{endpoint_id}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// Shipped with the release / site.
    Bootstrap,
    /// Added by the user by hand: kept above discovered ones.
    Manual,
    /// `/get_public_nodes` of a node in use.
    PublicNodes,
    /// Mainline DHT (apps only).
    Dht,
    /// `GetNodes` gossip.
    Gossip,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeRecord {
    pub addr: NodeAddr,
    pub source: Source,
    pub first_seen: u64,
    pub last_ok: u64,
    pub ewma_latency_ms: f64,
    /// EWMA of successes (starts at 0.5: unknown).
    pub ok_rate: f64,
    /// Last tip height this node reported.
    pub height: u64,
    pub strikes: u32,
    pub banned_until: u64,
    pub tls: Option<bool>,
}

impl NodeRecord {
    fn new(addr: NodeAddr, source: Source, now: u64) -> Self {
        Self { addr, source, first_seen: now, last_ok: 0, ewma_latency_ms: 0.0, ok_rate: 0.5, height: 0, strikes: 0, banned_until: 0, tls: None }
    }
    pub fn key(&self) -> String {
        self.addr.key()
    }
    pub fn is_banned(&self, now: u64) -> bool {
        now < self.banned_until
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NodeList {
    records: Vec<NodeRecord>,
}

impl NodeList {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    pub fn records(&self) -> &[NodeRecord] {
        &self.records
    }
    pub fn get(&self, key: &str) -> Option<&NodeRecord> {
        self.records.iter().find(|r| r.key() == key)
    }
    fn get_mut(&mut self, key: &str) -> Option<&mut NodeRecord> {
        self.records.iter_mut().find(|r| r.key() == key)
    }

    /// Adds a candidate (or refreshes the address of a known one; a manual source is never downgraded).
    /// At the cap ([`MAX_NODES`]) the worst non-bootstrap, non-manual record makes room; if there is
    /// none the candidate is dropped. Returns whether the list changed.
    pub fn add(&mut self, addr: NodeAddr, source: Source, now: u64) -> bool {
        let key = addr.key();
        if let Some(r) = self.get_mut(&key) {
            if r.addr != addr {
                r.addr = addr;
                return true;
            }
            if source == Source::Manual && r.source != Source::Manual {
                r.source = Source::Manual;
                return true;
            }
            return false;
        }
        if self.records.len() >= MAX_NODES as usize {
            let best = self.best_height(now);
            let victim = self
                .records
                .iter()
                .enumerate()
                .filter(|(_, r)| !matches!(r.source, Source::Bootstrap | Source::Manual))
                .min_by(|a, b| self.score(a.1, now, best).total_cmp(&self.score(b.1, now, best)))
                .map(|(i, _)| i);
            match victim {
                Some(i) => {
                    self.records.swap_remove(i);
                }
                None => return false,
            }
        }
        self.records.push(NodeRecord::new(addr, source, now));
        true
    }

    pub fn remove(&mut self, key: &str) {
        self.records.retain(|r| r.key() != key);
    }

    /// A call to `key` succeeded in `latency_ms`; `height` is its tip height if the call told.
    pub fn record_ok(&mut self, key: &str, latency_ms: f64, height: Option<u64>, now: u64) {
        if let Some(r) = self.get_mut(key) {
            r.ewma_latency_ms = if r.last_ok == 0 { latency_ms } else { r.ewma_latency_ms * (1.0 - EWMA_ALPHA) + latency_ms * EWMA_ALPHA };
            r.ok_rate = r.ok_rate * (1.0 - EWMA_ALPHA) + EWMA_ALPHA;
            r.last_ok = now;
            r.strikes = 0;
            if let Some(h) = height {
                r.height = h;
            }
        }
    }

    /// A call failed (timeout, transport error). Three strikes in a row bans for an hour.
    pub fn record_err(&mut self, key: &str, now: u64) {
        if let Some(r) = self.get_mut(key) {
            r.ok_rate *= 1.0 - EWMA_ALPHA;
            r.strikes += 1;
            if r.strikes >= 3 {
                r.banned_until = r.banned_until.max(now + 3600);
            }
        }
    }

    /// A node caught lying (bad block id, template on the wrong parent, proof that does not replay): a week.
    pub fn ban_liar(&mut self, key: &str, now: u64) {
        if let Some(r) = self.get_mut(key) {
            r.banned_until = now + LIAR_BAN_SECS;
            r.ok_rate = 0.0;
        }
    }

    pub fn set_tls(&mut self, key: &str, tls: bool) {
        if let Some(r) = self.get_mut(key) {
            r.tls = Some(tls);
        }
    }

    /// The reference tip height for the lag rule: the (lower) median of the heights reported by non-banned nodes that
    /// answered. Not the maximum: one node claiming a huge height must not make every honest node look behind (and so
    /// leave only itself to be picked).
    pub fn best_height(&self, now: u64) -> u64 {
        let mut h: Vec<u64> = self.records.iter().filter(|r| r.last_ok > 0 && !r.is_banned(now)).map(|r| r.height).collect();
        if h.is_empty() {
            return 0;
        }
        h.sort_unstable();
        h[(h.len() - 1) / 2]
    }

    /// A list read back from storage that the page (or anything else) could have written: only records for `keep` keys
    /// survive (the browser: its bootstrap entries), with their latency and success history; bans, strikes, heights and
    /// sources are not taken from storage (they are learned again within a minute), so a poisoned list cannot ban the
    /// real nodes, make them look behind, or add nodes of its own.
    pub fn sanitize_loaded(&mut self, keep: &[String], now: u64) {
        self.records.retain(|r| keep.contains(&r.key()));
        for r in &mut self.records {
            r.banned_until = 0;
            r.strikes = 0;
            r.height = 0;
            r.source = Source::Bootstrap;
            r.last_ok = r.last_ok.min(now);
            r.first_seen = r.first_seen.min(now);
            r.ok_rate = if r.ok_rate.is_finite() { r.ok_rate.clamp(0.0, 1.0) } else { 0.5 };
            r.ewma_latency_ms = if r.ewma_latency_ms.is_finite() { r.ewma_latency_ms.clamp(0.0, 60_000.0) } else { 0.0 };
        }
    }

    /// ok_rate x latency factor x freshness (x 1.25 for hand-added nodes), minus a penalty per
    /// block of height lag. Higher is better.
    pub fn score(&self, r: &NodeRecord, now: u64, best_height: u64) -> f64 {
        let latency = if r.last_ok == 0 { 0.5 } else { 1.0 / (1.0 + r.ewma_latency_ms / 500.0) };
        let age = now.saturating_sub(r.last_ok) as f64;
        let fresh = if r.last_ok == 0 { 0.7 } else { 1.0 / (1.0 + age / 86_400.0) };
        let lag = best_height.saturating_sub(r.height) as f64;
        let manual = if r.source == Source::Manual { 1.25 } else { 1.0 };
        (r.ok_rate * latency * fresh * manual - 0.1 * lag).max(0.0)
    }

    /// Usable nodes, best first: not banned, not more than [`MAX_HEIGHT_LAG`] blocks behind the best
    /// tip seen (a never-asked node, height 0, is kept: it has no lag evidence yet).
    pub fn candidates(&self, now: u64) -> Vec<(&NodeRecord, f64)> {
        let best = self.best_height(now);
        let mut v: Vec<_> = self
            .records
            .iter()
            .filter(|r| !r.is_banned(now))
            .filter(|r| r.last_ok == 0 || best.saturating_sub(r.height) <= MAX_HEIGHT_LAG)
            .map(|r| (r, self.score(r, now, best)))
            .collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        v
    }

    /// One node: weighted-random among the top [`PICK_TOP`] (weights = scores), so load spreads.
    pub fn pick(&self, now: u64, rand: u64) -> Option<&NodeRecord> {
        self.pick_many(1, now, rand).into_iter().next()
    }

    /// `n` distinct nodes by repeated weighted picks (for submit fan-out and the failover order).
    pub fn pick_many(&self, n: usize, now: u64, mut rand: u64) -> Vec<&NodeRecord> {
        let mut pool: Vec<(&NodeRecord, f64)> = self.candidates(now);
        let mut out = Vec::new();
        while out.len() < n && !pool.is_empty() {
            let top = pool.len().min(PICK_TOP);
            let total: f64 = pool[..top].iter().map(|p| p.1 + 1e-6).sum();
            rand = rand.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let mut x = (rand >> 11) as f64 / (1u64 << 53) as f64 * total;
            let mut idx = top - 1;
            for (i, p) in pool[..top].iter().enumerate() {
                x -= p.1 + 1e-6;
                if x <= 0.0 {
                    idx = i;
                    break;
                }
            }
            out.push(pool.remove(idx).0);
        }
        out
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("node list serializes")
    }

    /// Loads a persisted list; keeps at most [`MAX_NODES`] records and drops duplicate keys.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        let mut l: NodeList = serde_json::from_str(s)?;
        let mut seen = std::collections::HashSet::new();
        l.records.retain(|r| seen.insert(r.key()));
        l.records.truncate(MAX_NODES as usize);
        Ok(l)
    }
}

/// The bootstrap list shipped with a release: `{"nodes": [..]}` or the old `{"url": ".."}`; entries are
/// https/http URLs, or `iroh:<endpoint id hex>[@<relay url>]`.
pub fn parse_bootstrap(json: &str) -> Result<Vec<NodeAddr>, serde_json::Error> {
    #[derive(Deserialize)]
    struct B {
        nodes: Option<Vec<String>>,
        url: Option<String>,
    }
    let b: B = serde_json::from_str(json)?;
    let mut v: Vec<String> = b.nodes.unwrap_or_default();
    v.extend(b.url);
    Ok(v.into_iter().filter_map(|s| parse_node_spec(&s)).collect())
}

/// `https://host`, `http://host:port`, or `iroh:<hex>[@<relay url>]`.
pub fn parse_node_spec(s: &str) -> Option<NodeAddr> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("iroh:") {
        let (id, relay) = match rest.split_once('@') {
            Some((i, r)) => (i, Some(r.to_string())),
            None => (rest, None),
        };
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        return Some(NodeAddr::Iroh { endpoint_id: id.to_ascii_lowercase(), relay_url: relay, direct: vec![] });
    }
    if (s.starts_with("https://") || s.starts_with("http://")) && !s.contains(['@', '?', '#', ' ']) {
        return Some(NodeAddr::Http { url: s.trim_end_matches('/').to_string() });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(n: u32) -> NodeAddr {
        NodeAddr::Http { url: format!("https://n{n}.example") }
    }

    #[test]
    fn add_dedups_and_caps() {
        let mut l = NodeList::new();
        assert!(l.add(http(0), Source::Bootstrap, 1));
        assert!(!l.add(http(0), Source::Gossip, 1));
        for i in 1..200 {
            l.add(http(i), Source::Gossip, 1);
        }
        assert_eq!(l.len(), MAX_NODES as usize);
        assert!(l.get(&http(0).key()).is_some(), "bootstrap entries are never evicted");
    }

    #[test]
    fn scoring_prefers_fast_reliable_and_skips_laggards() {
        let mut l = NodeList::new();
        for i in 0..3 {
            l.add(http(i), Source::Bootstrap, 0);
        }
        let now = 1000;
        for _ in 0..5 {
            l.record_ok(&http(0).key(), 50.0, Some(100), now);
            l.record_ok(&http(1).key(), 900.0, Some(100), now);
            l.record_ok(&http(2).key(), 50.0, Some(90), now);
        }
        let c = l.candidates(now);
        assert_eq!(c[0].0.key(), http(0).key());
        assert!(c.iter().all(|(r, _)| r.key() != http(2).key()), "10 blocks behind is skipped");
    }

    #[test]
    fn one_node_claiming_a_huge_height_does_not_push_the_others_out() {
        let mut l = NodeList::new();
        for i in 0..3 {
            l.add(http(i), Source::Bootstrap, 0);
        }
        for i in 0..2 {
            l.record_ok(&http(i).key(), 50.0, Some(100), 10);
        }
        l.record_ok(&http(2).key(), 50.0, Some(1 << 40), 10);
        let c = l.candidates(10);
        assert_eq!(c.len(), 3, "the honest pair is not 'behind' the liar's claim");
        // and a stale node is still skipped
        l.record_ok(&http(1).key(), 50.0, Some(90), 10);
        l.add(http(3), Source::Bootstrap, 0);
        l.record_ok(&http(3).key(), 50.0, Some(100), 10);
        assert!(l.candidates(10).iter().all(|(r, _)| r.key() != http(1).key()));
    }

    #[test]
    fn a_stored_list_cannot_ban_add_or_demote_nodes() {
        let mut l = NodeList::new();
        for i in 0..3 {
            l.add(http(i), Source::Gossip, 0);
        }
        l.ban_liar(&http(0).key(), 5);
        l.record_ok(&http(1).key(), 10.0, Some(1 << 40), 5);
        let mut loaded = NodeList::from_json(&l.to_json()).unwrap();
        loaded.sanitize_loaded(&[http(0).key(), http(1).key()], 6);
        assert_eq!(loaded.len(), 2, "records for nodes not in the bootstrap list are dropped");
        assert!(loaded.records().iter().all(|r| !r.is_banned(6) && r.height == 0 && r.source == Source::Bootstrap));
        assert_eq!(loaded.get(&http(1).key()).unwrap().ewma_latency_ms, 10.0, "the score history stays");
    }

    #[test]
    fn errors_ban_and_liars_ban_for_a_week() {
        let mut l = NodeList::new();
        l.add(http(0), Source::Bootstrap, 0);
        l.add(http(1), Source::Bootstrap, 0);
        for _ in 0..3 {
            l.record_err(&http(0).key(), 10);
        }
        assert_eq!(l.candidates(11).len(), 1);
        assert_eq!(l.candidates(10 + 3601).len(), 2);
        l.ban_liar(&http(1).key(), 10);
        assert!(l.get(&http(1).key()).unwrap().is_banned(10 + LIAR_BAN_SECS - 1));
        assert!(!l.get(&http(1).key()).unwrap().is_banned(10 + LIAR_BAN_SECS));
    }

    #[test]
    fn pick_spreads_over_top_and_is_distinct() {
        let mut l = NodeList::new();
        for i in 0..8 {
            l.add(http(i), Source::Bootstrap, 0);
            l.record_ok(&http(i).key(), 100.0, Some(5), 10);
        }
        let mut seen = std::collections::HashSet::new();
        for r in 0..200u64 {
            seen.insert(l.pick(10, r.wrapping_mul(0x9E3779B97F4A7C15)).unwrap().key());
        }
        assert!(seen.len() >= 3 && seen.len() <= PICK_TOP, "{}", seen.len());
        let three = l.pick_many(3, 10, 7);
        let keys: std::collections::HashSet<_> = three.iter().map(|r| r.key()).collect();
        assert_eq!(keys.len(), 3);
    }

    #[test]
    fn json_roundtrip_and_bootstrap_formats() {
        let mut l = NodeList::new();
        l.add(http(0), Source::Manual, 5);
        let l2 = NodeList::from_json(&l.to_json()).unwrap();
        assert_eq!(l2.len(), 1);
        let id = "ab".repeat(32);
        let b = parse_bootstrap(&format!(r#"{{"nodes":["https://a.example/","iroh:{id}@https://r.example"],"url":"https://eboy.au"}}"#)).unwrap();
        assert_eq!(b.len(), 3);
        assert!(parse_bootstrap(r#"{"url":"https://eboy.au"}"#).unwrap().len() == 1);
        assert!(parse_node_spec("iroh:zz").is_none());
        assert!(parse_node_spec("ftp://x").is_none());
    }
}
