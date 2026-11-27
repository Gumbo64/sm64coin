//! The tip over plain HTTP(S): Server-Sent Events at `GET /lc/tip`, next to the iroh `SubscribeTip` (server.rs) and fed by the same
//! [`TipFeed`] (one watcher of the node for every subscriber, whatever the transport).
//!
//! The service speaks plain HTTP on a local port (`--sse-bind 127.0.0.1:PORT`); the public HTTPS address is the node's reverse proxy
//! (Caddy / nginx) forwarding `/lc/tip` to it with buffering OFF (Caddy: `reverse_proxy /lc/tip 127.0.0.1:PORT { flush_interval -1 }`;
//! nginx: `proxy_buffering off; proxy_read_timeout 1h;`). Browsers need CORS: the response carries `Access-Control-Allow-Origin: *`
//! (the stream is public chain data), and the site's CSP `connect-src` already allows `https:` (EventSource is a connect-src fetch).
//!
//! Wire: `text/event-stream`; an unnamed event whose data is the JSON tip record ([`crate::exec::tip_record`]) for the current tip and
//! each change; `event: ka` with `data: 1` every keepalive period. Caps: subscriptions per client IP (behind a proxy the IP is read from
//! `X-Forwarded-For` when `trust_forwarded`, which is only safe when the listener is not reachable except through that proxy) and in total.
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::exec::tip_record;
use crate::tipfeed::TipFeed;

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

const CORS: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\n";

/// Binds and serves until the returned task is aborted; returns the bound address and the task.
pub async fn serve(feed: Arc<TipFeed>, cfg: SseConfig) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let l = TcpListener::bind(cfg.bind).await?;
    let addr = l.local_addr()?;
    let h = tokio::spawn(async move {
        loop {
            let Ok((sock, peer)) = l.accept().await else { continue };
            let (feed, cfg) = (feed.clone(), cfg.clone());
            tokio::spawn(async move {
                let _ = handle(sock, peer.ip(), feed, cfg).await;
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
    let Some(_sub) = feed.acquire(&format!("ip:{ip}"), cfg.max_per_ip) else {
        return sock.write_all(format!("HTTP/1.1 429 Too Many Requests\r\n{CORS}Retry-After: 30\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
    };
    sock.write_all(format!("HTTP/1.1 200 OK\r\n{CORS}Content-Type: text/event-stream\r\nCache-Control: no-cache\r\nX-Accel-Buffering: no\r\nConnection: close\r\n\r\nretry: 5000\n\n").as_bytes()).await?;
    let (mut rd, mut wr) = sock.into_split();
    let mut rx = feed.subscribe();
    let mut last: Option<[u8; 32]> = None;
    let mut sink = [0u8; 64];
    loop {
        let cur = rx.borrow_and_update().clone();
        if let Some(t) = cur.filter(|t| Some(t.hash) != last) {
            last = Some(t.hash);
            wr.write_all(format!("data: {}\n\n", tip_record("", &t)).as_bytes()).await?;
        }
        tokio::select! {
            r = rx.changed() => if r.is_err() { return Ok(()) },
            _ = tokio::time::sleep(feed.cfg.keepalive) => wr.write_all(b"event: ka\ndata: 1\n\n").await?,
            n = rd.read(&mut sink) => if n? == 0 { return Ok(()) },
        }
    }
}
