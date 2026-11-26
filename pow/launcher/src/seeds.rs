//! Seed peers: seeds.txt parsing and name resolution.
use std::fs;
use std::path::Path;

/// `host:port` (port 16480 if missing) as `ip:port`, or None (with a note) if it doesn't resolve.
pub fn resolve_peer(p: &str) -> Option<String> {
    use std::net::ToSocketAddrs;
    let with_port = if p.rsplit_once(':').is_some_and(|(_, port)| port.parse::<u16>().is_ok()) { p.to_string() } else { format!("{p}:16480") };
    match with_port.to_socket_addrs() {
        Ok(mut a) => match a.find(|x| x.is_ipv4()) {
            Some(x) => Some(x.to_string()),
            None => {
                println!("seed {p}: no IPv4 address, skipped");
                None
            }
        },
        Err(e) => {
            println!("seed {p} doesn't resolve ({e}), skipped");
            None
        }
    }
}

pub fn read_seeds(p: &Path) -> Vec<String> {
    fs::read_to_string(p)
        .map(|s| s.lines().map(|l| l.split('#').next().unwrap_or("").split_whitespace().collect::<String>()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}
