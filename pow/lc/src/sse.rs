//! The tip over plain HTTP(S): Server-Sent Events at `GET /lc/tip`, fed by a [`TipFeed`] (one watcher of the node for every
//! subscriber).
//!
//! The service speaks plain HTTP on a local port (`--sse-bind 127.0.0.1:PORT`); the public HTTPS address is the node's reverse proxy
//! (Caddy / nginx) forwarding `/lc/tip` to it with buffering OFF (Caddy: `reverse_proxy /lc/tip 127.0.0.1:PORT { flush_interval -1 }`;
//! nginx: `proxy_buffering off; proxy_read_timeout 1h;`). Browsers need CORS: the response carries `Access-Control-Allow-Origin: *`
//! (the stream is public chain data), and the site's CSP `connect-src` already allows `https:` (EventSource is a connect-src fetch).
//!
//! Wire: `text/event-stream`; an unnamed event whose data is the JSON tip record ([`crate::exec::tip_record`]) for the current tip and
//! each change, its `server_time` this server's clock at sending; `event: ka` whose data is that clock (unix s) every keepalive period
//! (a client corrects its own clock by it: a held run is sent by the node's clock). Caps: subscriptions per client IP (an IPv6 client
//! per /64; behind a proxy the IP is read from `X-Forwarded-For` when `trust_forwarded`, which is only safe when the listener is not
//! reachable except through that proxy: a loopback bind) and in total, and open connections in all (a slow or idle client cannot
//! pile up tasks). A reader that stops reading blocks only its own task (the tip is a `watch`: nothing queues up) and is dropped
//! when a write has not gone through within [`WRITE_TIMEOUT`].
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::exec::tip_record;
use crate::tipfeed::{TipFeed, now_secs};

fn peer_port_salt(s: &TcpStream) -> u32 {
    s.peer_addr().map(|a| a.port() as u32 * 7919).unwrap_or(0)
}

pub use crate::http::SSE_PATH;

#[derive(Clone, Debug)]
pub struct SseConfig {
    pub bind: SocketAddr,
    pub trust_forwarded: bool,
    pub max_per_ip: usize,
}

impl Default for SseConfig {
    fn default() -> Self {
        Self { bind: ([127, 0, 0, 1], 0).into(), trust_forwarded: false, max_per_ip: 4 }
    }
}

impl SseConfig {
    /// The usual setup: `X-Forwarded-For` trusted exactly when the listener is on loopback (only a local reverse proxy reaches
    /// it; without that every visitor would be 127.0.0.1 and share one IP's few slots).
    pub fn behind_local_proxy(bind: SocketAddr) -> Self {
        Self { bind, trust_forwarded: bind.ip().is_loopback(), ..Default::default() }
    }
}

/// A write (an event, a keepalive) that has not gone through in this long drops the subscriber: its reader stopped.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Open connections the listener serves at once beyond the subscription cap (requests still sending their head, 404s, 429s).
const EXTRA_CONNS: usize = 256;

/// The key a client's subscriptions are counted under: its IP, an IPv6 one by its /64 (one host has a whole /64).
fn ip_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V6(v6) if v6.to_ipv4_mapped().is_none() => {
            let s = v6.segments();
            format!("ip6:{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        IpAddr::V6(v6) => format!("ip:{}", v6.to_ipv4_mapped().unwrap()),
        IpAddr::V4(v4) => format!("ip:{v4}"),
    }
}

async fn timed_write(wr: &mut tokio::net::tcp::OwnedWriteHalf, b: &[u8]) -> std::io::Result<()> {
    tokio::time::timeout(WRITE_TIMEOUT, wr.write_all(b)).await.map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))?
}

const CORS: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\n";

/// Binds and serves until the returned task is aborted; returns the bound address and the task.
pub async fn serve(feed: Arc<TipFeed>, cfg: SseConfig) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let l = TcpListener::bind(cfg.bind).await?;
    let addr = l.local_addr()?;
    let conns = Arc::new(tokio::sync::Semaphore::new(feed.cfg.max_total + EXTRA_CONNS));
    let h = tokio::spawn(async move {
        loop {
            let (sock, peer) = match l.accept().await {
                Ok(x) => x,
                Err(_) => {
                    // out of file descriptors and the like: accept fails at once again, so do not spin on it
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            // over the connection cap: close at once (the browser retries later by itself)
            let Ok(permit) = conns.clone().try_acquire_owned() else { continue };
            let (feed, cfg) = (feed.clone(), cfg.clone());
            tokio::spawn(async move {
                let _ = handle(sock, peer.ip(), feed, cfg).await;
                drop(permit);
            });
        }
    });
    Ok((addr, h))
}

async fn handle(mut sock: TcpStream, peer: IpAddr, feed: Arc<TipFeed>, cfg: SseConfig) -> std::io::Result<()> {
    // the request head, at most 8 KiB, within 10 s
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    let read_head = async {
        while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 8192 {
            let n = sock.read(&mut buf).await?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            head.extend_from_slice(&buf[..n]);
        }
        Ok::<(), std::io::Error>(())
    };
    tokio::time::timeout(Duration::from_secs(10), read_head).await.map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
    let text = String::from_utf8_lossy(&head).to_string();
    let first = text.lines().next().unwrap_or("");
    let (method, path) = {
        let mut p = first.split_whitespace();
        (p.next().unwrap_or(""), p.next().unwrap_or(""))
    };
    if method == "OPTIONS" {
        return sock.write_all(format!("HTTP/1.1 204 No Content\r\n{CORS}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
    }
    if method != "GET" || path.split('?').next() != Some(SSE_PATH) {
        return sock.write_all(format!("HTTP/1.1 404 Not Found\r\n{CORS}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
    }
    let ip = if cfg.trust_forwarded {
        text.lines()
            .find_map(|l| l.to_ascii_lowercase().strip_prefix("x-forwarded-for:").map(|v| v.to_string()))
            .and_then(|v| v.split(',').next_back().and_then(|x| x.trim().parse::<IpAddr>().ok()))
            .unwrap_or(peer)
    } else {
        peer
    };
    let Some(_sub) = feed.acquire(&ip_key(ip), cfg.max_per_ip) else {
        return sock.write_all(format!("HTTP/1.1 429 Too Many Requests\r\n{CORS}Retry-After: 30\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
    };
    // The browser reconnects by itself after `retry` ms: spread over 4-10 s, so a restart does not bring every page back at once.
    let retry = 4000 + (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0) ^ (peer_port_salt(&sock))) % 6000;
    let (mut rd, mut wr) = sock.into_split();
    timed_write(&mut wr, format!("HTTP/1.1 200 OK\r\n{CORS}Content-Type: text/event-stream\r\nCache-Control: no-cache, no-transform\r\nX-Accel-Buffering: no\r\nConnection: close\r\n\r\nretry: {retry}\n\n").as_bytes()).await?;
    let mut rx = feed.subscribe();
    let mut last: Option<[u8; 32]> = None;
    let mut sink = [0u8; 64];
    loop {
        let cur = rx.borrow_and_update().clone();
        if let Some(t) = cur.filter(|t| Some(t.hash) != last) {
            last = Some(t.hash);
            // the server's clock now, not when the tip was first seen (a subscriber may come long after)
            let t = crate::proto::TipInfo { server_time: now_secs(), ..t };
            timed_write(&mut wr, format!("data: {}\n\n", tip_record("", &t)).as_bytes()).await?;
        }
        tokio::select! {
            r = rx.changed() => if r.is_err() { return Ok(()) },
            _ = tokio::time::sleep(feed.cfg.keepalive) => timed_write(&mut wr, format!("event: ka\ndata: {}\n\n", now_secs()).as_bytes()).await?,
            n = rd.read(&mut sink) => if n? == 0 { return Ok(()) },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscriptions_are_counted_per_ipv4_address_and_per_ipv6_64() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::9".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(ip_key(a), ip_key(b), "one host, one /64");
        assert_ne!(ip_key(a), ip_key(c));
        assert_eq!(ip_key("::ffff:192.0.2.7".parse().unwrap()), ip_key("192.0.2.7".parse().unwrap()));
        assert_ne!(ip_key("192.0.2.7".parse().unwrap()), ip_key("192.0.2.8".parse().unwrap()));
    }

    #[test]
    fn a_loopback_listener_trusts_its_proxy() {
        assert!(SseConfig::behind_local_proxy("127.0.0.1:16490".parse().unwrap()).trust_forwarded);
        assert!(SseConfig::behind_local_proxy("[::1]:16490".parse().unwrap()).trust_forwarded);
        assert!(!SseConfig::behind_local_proxy("0.0.0.0:16490".parse().unwrap()).trust_forwarded);
    }
}
