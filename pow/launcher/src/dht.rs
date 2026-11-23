//! Finding SM64Coin nodes through BitTorrent's Mainline DHT: nodes that accept
//! connections announce their p2p port under one network key, and a starting
//! node looks the key up and passes what it finds to `sm64coind` as
//! `--add-peer`. After that the node's own peer exchange takes over, so the
//! DHT matters mostly on a first start. No seed operator, no addresses in the
//! repository. Anyone can announce, so the answers may include junk or
//! hostile nodes: the node's handshake drops other networks, every block is
//! verified, and `seeds.txt` stays a second source.
#![allow(deprecated)] // mainline's blocking API

use mainline::{Dht, Id};
use sha1::{Digest, Sha1};
use std::net::SocketAddrV4;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The network key's name; a new network (new genesis) takes a new one.
pub const NETWORK: &str = "sm64coin-mainnet-1";

/// The key: SHA-1 of the network's name (`SM64COIN_DHT_NETWORK` overrides it, for tests).
pub fn info_hash() -> Id {
    let name = std::env::var("SM64COIN_DHT_NETWORK").unwrap_or_else(|_| NETWORK.to_string());
    Id::from_bytes(Sha1::digest(name.as_bytes())).expect("sha1 is 20 bytes")
}

/// Up to `max` announced nodes, within `timeout`; empty when the DHT can't be reached.
pub fn lookup(timeout: Duration, max: usize) -> Vec<SocketAddrV4> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok(dht) = Dht::client() else { return };
        for batch in dht.get_peers(info_hash()) {
            if tx.send(batch).is_err() {
                return;
            }
        }
    });
    let end = Instant::now() + timeout;
    let mut out: Vec<SocketAddrV4> = Vec::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(batch) => {
                for a in batch {
                    if !out.contains(&a) && !a.ip().is_unspecified() && a.port() != 0 {
                        out.push(a);
                    }
                }
                if out.len() >= max {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    out.truncate(max);
    out
}

/// Announces `port` (this machine's public IP, as the DHT sees it) every 15 minutes, forever.
pub fn announce_forever(port: u16) {
    std::thread::spawn(move || {
        let Ok(dht) = Dht::client() else { return };
        loop {
            match dht.announce_peer(info_hash(), Some(port)) {
                Ok(_) => println!("DHT: announced this node (p2p port {port})."),
                Err(e) => println!("DHT: announce failed ({e}); retrying later."),
            }
            std::thread::sleep(Duration::from_secs(15 * 60));
        }
    });
}
