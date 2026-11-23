//! Sharing the top entries between leaderboard servers. Pull based (works behind NAT):
//! every server serves `GET /share` and periodically pulls it from its peers.
use crate::scan::{State, now};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const MAX_PEERS: usize = 200;
const MAX_RESPONSE: u64 = 256 * 1024;

pub struct PeerInfo {
    /// "config", "daemon" (a connected node of ours) or "inbound" (announced itself when pulling from us)
    pub source: &'static str,
    pub ok: bool,
    pub last_ok: u64,
    pub fails: u32,
    pub next_try: u64,
    pub entries: usize,
}

#[derive(Default)]
pub struct Peers {
    pub map: BTreeMap<String, PeerInfo>,
}

impl Peers {
    pub fn add(&mut self, url: &str, source: &'static str) -> bool {
        let Some(url) = norm_url(url) else { return false };
        if self.map.contains_key(&url) || self.map.len() >= MAX_PEERS {
            return false;
        }
        self.map.insert(url, PeerInfo { source, ok: false, last_ok: 0, fails: 0, next_try: 0, entries: 0 });
        true
    }
    pub fn active(&self) -> usize {
        self.map.values().filter(|p| p.ok).count()
    }
    pub fn json(&self) -> Value {
        let list: Vec<Value> = self.map.iter().map(|(u, p)| json!({"url": u, "source": p.source, "ok": p.ok, "last_ok": p.last_ok, "entries": p.entries, "fails": p.fails})).collect();
        json!({"active": self.active(), "known": self.map.len(), "peers": list})
    }
}

/// `host:port` or `http://host:port[/]` to `http://host:port`; anything else is rejected.
pub fn norm_url(s: &str) -> Option<String> {
    let s = s.trim().trim_end_matches('/');
    let rest = s.strip_prefix("http://").or_else(|| s.strip_prefix("https://")).map(|r| (s.split("://").next().unwrap(), r)).unwrap_or(("http", s));
    let (scheme, hostport) = rest;
    if hostport.is_empty() || hostport.len() > 100 || hostport.contains(['/', '?', '#', '@', ' ']) {
        return None;
    }
    Some(format!("{scheme}://{hostport}"))
}

/// Peer URLs from a file: one per line, `#` comments.
pub fn read_peers_file(p: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(p).map(|t| t.lines().map(|l| l.split('#').next().unwrap_or("").trim().to_string()).filter(|l| !l.is_empty()).collect()).unwrap_or_default()
}

/// IPs of the daemon's connected peers (RPC `get_connections`; needs unrestricted RPC).
pub fn parse_connections(result: &Value) -> Vec<IpAddr> {
    let mut v = vec![];
    for c in result["connections"].as_array().into_iter().flatten() {
        let ip = c["ip"].as_str().or_else(|| c["host"].as_str()).and_then(|s| s.parse::<IpAddr>().ok());
        if let Some(ip) = ip {
            if !v.contains(&ip) {
                v.push(ip);
            }
        }
    }
    v
}

pub fn url_for_ip(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V6(a) => format!("http://[{a}]:{port}"),
        IpAddr::V4(a) => format!("http://{a}:{port}"),
    }
}

/// An inbound announcement (`X-Leaderboard-Url`) is only accepted when its host is the caller's own IP,
/// so announcing cannot make this server fetch from third parties.
pub fn announce_ok(announced: &str, remote: IpAddr) -> Option<String> {
    let url = norm_url(announced)?;
    let hostport = url.strip_prefix("http://")?;
    let sa: std::net::SocketAddr = hostport.parse().ok()?;
    (sa.ip() == remote).then_some(url)
}

pub struct Cfg {
    pub daemon: String,
    pub path: PathBuf,
    pub id: String,
    pub secs: u64,
    pub discover: bool,
    pub peer_port: u16,
    pub announce: Option<String>,
}

fn get_share(url: &str, cfg: &Cfg) -> Result<Value, String> {
    let mut r = ureq::get(&format!("{url}/share")).timeout(Duration::from_secs(10));
    if let Some(a) = &cfg.announce {
        r = r.set("X-Leaderboard-Url", a);
    }
    let resp = r.call().map_err(|e| e.to_string())?;
    let mut body = String::new();
    std::io::Read::read_to_string(&mut resp.into_reader().take(MAX_RESPONSE + 1), &mut body).map_err(|e| e.to_string())?;
    if body.len() as u64 > MAX_RESPONSE {
        return Err("response too large".into());
    }
    serde_json::from_str(&body).map_err(|e| e.to_string())
}
use std::io::Read;

/// One round: the node's own board first (nodes share it over p2p:
/// sm64_board), then discover peers, pull from each that is due, merge what
/// they share.
pub fn round(state: &Mutex<State>, peers: &Mutex<Peers>, cfg: &Cfg) {
    let board: Result<Value, String> = ureq::post(&format!("{}/json_rpc", cfg.daemon))
        .timeout(Duration::from_secs(10))
        .send_json(json!({"jsonrpc":"2.0","id":"0","method":"sm64_board"}))
        .map_err(|e| e.to_string())
        .and_then(|r| r.into_json().map_err(|e| e.to_string()));
    if let Ok(v) = board {
        if let Some(entries) = v["result"]["entries"].as_array() {
            let rep = state.lock().unwrap().merge_peer(entries, "node", &cfg.path);
            if rep.added > 0 {
                eprintln!("node board: {} added, {} known", rep.added, rep.known);
            }
        }
    }
    if cfg.discover {
        let r: Result<Value, String> = ureq::post(&format!("{}/json_rpc", cfg.daemon))
            .timeout(Duration::from_secs(10))
            .send_json(json!({"jsonrpc":"2.0","id":"0","method":"get_connections"}))
            .map_err(|e| e.to_string())
            .and_then(|r| r.into_json().map_err(|e| e.to_string()));
        if let Ok(v) = r {
            let mut p = peers.lock().unwrap();
            for ip in parse_connections(&v["result"]) {
                p.add(&url_for_ip(ip, cfg.peer_port), "daemon");
            }
        }
    }
    let due: Vec<String> = {
        let t = now();
        peers.lock().unwrap().map.iter().filter(|(_, p)| p.next_try <= t).map(|(u, _)| u.clone()).collect()
    };
    for url in due {
        let res = get_share(&url, cfg).and_then(|v| {
            let id = v["id"].as_str().unwrap_or("");
            if id == cfg.id {
                return Err("self".to_string());
            }
            v["entries"].as_array().cloned().ok_or_else(|| "no entries".to_string())
        });
        let t = now();
        let mut p = peers.lock().unwrap();
        match res {
            Ok(entries) => {
                let rep = state.lock().unwrap().merge_peer(&entries, &url, &cfg.path);
                if rep.added > 0 || rep.rejected > 0 {
                    eprintln!("share: {url}: {} entries, {} new, {} rejected", entries.len(), rep.added, rep.rejected);
                }
                if let Some(i) = p.map.get_mut(&url) {
                    *i = PeerInfo { source: i.source, ok: true, last_ok: t, fails: 0, next_try: 0, entries: entries.len() };
                }
            }
            Err(e) => {
                if e == "self" {
                    p.map.remove(&url);
                } else if let Some(i) = p.map.get_mut(&url) {
                    i.ok = false;
                    i.fails += 1;
                    i.next_try = t + (cfg.secs << i.fails.min(5)).min(6 * 3600);
                    if i.source != "config" && i.fails >= 8 {
                        p.map.remove(&url);
                    }
                }
            }
        }
    }
}

pub fn spawn(state: Arc<Mutex<State>>, peers: Arc<Mutex<Peers>>, cfg: Cfg) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        loop {
            round(&state, &peers, &cfg);
            // jitter +-20%
            let j = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0) % 41) as u64;
            std::thread::sleep(Duration::from_secs((cfg.secs * (80 + j) / 100).max(1)));
        }
    });
}

/// Per-IP rate limit for /share: at most one request per `gap` seconds.
#[derive(Default)]
pub struct RateLimit(std::collections::HashMap<IpAddr, std::time::Instant>);

impl RateLimit {
    pub fn allow(&mut self, ip: IpAddr, gap: Duration) -> bool {
        let now = std::time::Instant::now();
        if self.0.len() > 2000 {
            self.0.retain(|_, t| now.duration_since(*t) < gap);
        }
        match self.0.get(&ip) {
            Some(t) if now.duration_since(*t) < gap => false,
            _ => {
                self.0.insert(ip, now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(norm_url("1.2.3.4:16490/").unwrap(), "http://1.2.3.4:16490");
        assert_eq!(norm_url(" http://h:1 ").unwrap(), "http://h:1");
        assert!(norm_url("http://u@h:1").is_none());
        assert!(norm_url("http://h:1/x").is_none());
        assert!(norm_url("").is_none());
    }

    #[test]
    fn connections_and_announce() {
        let r = json!({"connections": [{"ip": "1.2.3.4", "host": "1.2.3.4", "port": "1"}, {"host": "5.6.7.8"}, {"host": "abc.onion"}, {"ip": "1.2.3.4"}]});
        let ips = parse_connections(&r);
        assert_eq!(ips.len(), 2);
        assert_eq!(url_for_ip(ips[0], 16490), "http://1.2.3.4:16490");
        assert_eq!(url_for_ip("::1".parse().unwrap(), 5), "http://[::1]:5");
        let remote: IpAddr = "1.2.3.4".parse().unwrap();
        assert!(announce_ok("http://1.2.3.4:16490", remote).is_some());
        assert!(announce_ok("http://10.0.0.1:16490", remote).is_none()); // someone else's host
        assert!(announce_ok("http://evil.example:80", remote).is_none());
    }

    #[test]
    fn peer_set_is_bounded_and_deduped() {
        let mut p = Peers::default();
        assert!(p.add("http://a:1", "config"));
        assert!(!p.add("a:1", "daemon"));
        for i in 0..MAX_PEERS * 2 {
            p.add(&format!("h{i}:1"), "daemon");
        }
        assert_eq!(p.map.len(), MAX_PEERS);
    }

    #[test]
    fn rate_limit() {
        let mut r = RateLimit::default();
        let ip: IpAddr = "9.9.9.9".parse().unwrap();
        assert!(r.allow(ip, Duration::from_secs(60)));
        assert!(!r.allow(ip, Duration::from_secs(60)));
        assert!(r.allow("9.9.9.8".parse().unwrap(), Duration::from_secs(60)));
    }
}
