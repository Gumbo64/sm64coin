//! The tip over plain HTTP(S): Server-Sent Events at `GET /lc/tip`, fed by a [`TipFeed`] (one watcher of the node for every
//! subscriber).
//!
//! Two ways to be reached. Behind a reverse proxy: plain HTTP on a local port, the proxy is the public address. Or directly: with
//! `tls` set the listener speaks HTTPS itself (rustls) on a public port (a node on a Raspberry Pi behind a router forward needs no
//! proxy), the certificate and key are PEM files that are reloaded when they change on disk (a certbot renewal needs no restart)
//! and the client IP for the caps is the TCP peer's (no `X-Forwarded-For` is read: nothing trusted stands in front).
//!
//! Behind a proxy: the service speaks plain HTTP on a local port (`--sse-bind 127.0.0.1:PORT`); the public HTTPS address is the node's reverse proxy
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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::exec::tip_record;
use crate::tipfeed::{TipFeed, now_secs};

pub use crate::http::SSE_PATH;

#[derive(Clone, Debug)]
pub struct SseConfig {
    pub bind: SocketAddr,
    /// Read the client IP from `X-Forwarded-For` (ignored when `tls` is set: the listener is then reached directly).
    pub trust_forwarded: bool,
    pub max_per_ip: usize,
    /// Serve HTTPS with this certificate (hot-reloaded); None: plain HTTP.
    pub tls: Option<Arc<TlsReloader>>,
    /// How often the certificate files are looked at for a change (also [`TlsReloader::reload_if_changed`] by hand, e.g. on SIGHUP).
    pub tls_check: Duration,
}

impl Default for SseConfig {
    fn default() -> Self {
        Self { bind: ([127, 0, 0, 1], 0).into(), trust_forwarded: false, max_per_ip: 4, tls: None, tls_check: Duration::from_secs(300) }
    }
}

/// How long a TLS handshake may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

type Stamp = Vec<Option<(SystemTime, u64)>>;

/// The certificate chain and key of the HTTPS listener (PEM files), swapped in place when the files change: handshakes already
/// running and open streams keep the old certificate, new ones get the new. A reload that fails (half-written files, a key that
/// does not match) keeps the old certificate.
pub struct TlsReloader {
    cert: PathBuf,
    key: PathBuf,
    cur: RwLock<Arc<rustls::sign::CertifiedKey>>,
    stamp: Mutex<Stamp>,
    reloads: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for TlsReloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TlsReloader({}, {})", self.cert.display(), self.key.display())
    }
}

fn stamp_of(paths: [&Path; 2]) -> Stamp {
    paths.iter().map(|p| std::fs::metadata(p).and_then(|m| Ok((m.modified()?, m.len()))).ok()).collect()
}

fn load_certified(cert: &Path, key: &Path) -> Result<rustls::sign::CertifiedKey, String> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    let certs = CertificateDer::pem_file_iter(cert)
        .map_err(|e| format!("{}: {e}", cert.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{}: {e}", cert.display()))?;
    if certs.is_empty() {
        return Err(format!("{}: no certificate in it", cert.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("{}: {e}", key.display()))?;
    let signing = rustls::crypto::ring::sign::any_supported_type(&key).map_err(|e| format!("private key: {e}"))?;
    let ck = rustls::sign::CertifiedKey::new(certs, signing);
    ck.keys_match().map_err(|e| format!("the key does not match the certificate: {e}"))?;
    Ok(ck)
}

impl TlsReloader {
    /// Reads the PEM certificate chain and private key now (an error when they are unreadable or do not match).
    pub fn load(cert: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Result<Arc<Self>, String> {
        let (cert, key) = (cert.into(), key.into());
        let stamp = stamp_of([&cert, &key]);
        let ck = load_certified(&cert, &key)?;
        Ok(Arc::new(Self { cert, key, cur: RwLock::new(Arc::new(ck)), stamp: Mutex::new(stamp), reloads: Default::default() }))
    }

    /// Re-reads the files when their modification time or size changed since the last look. Ok(true): a new certificate is in use.
    pub fn reload_if_changed(&self) -> Result<bool, String> {
        let now = stamp_of([&self.cert, &self.key]);
        if *self.stamp.lock().unwrap() == now {
            return Ok(false);
        }
        self.reload_stamped(now).map(|_| true)
    }

    /// Re-reads the files now (SIGHUP).
    pub fn reload(&self) -> Result<(), String> {
        self.reload_stamped(stamp_of([&self.cert, &self.key]))
    }

    fn reload_stamped(&self, stamp: Stamp) -> Result<(), String> {
        let ck = load_certified(&self.cert, &self.key)?;
        *self.cur.write().unwrap() = Arc::new(ck);
        *self.stamp.lock().unwrap() = stamp;
        self.reloads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// How many times a new certificate was swapped in since the first load.
    pub fn reloads(&self) -> u64 {
        self.reloads.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn acceptor(self: &Arc<Self>) -> tokio_rustls::TlsAcceptor {
        let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default protocol versions")
            .with_no_client_auth()
            .with_cert_resolver(self.clone());
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        tokio_rustls::TlsAcceptor::from(Arc::new(cfg))
    }
}

impl rustls::server::ResolvesServerCert for TlsReloader {
    fn resolve(&self, _: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.cur.read().unwrap().clone())
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

async fn timed_write<W: AsyncWrite + Unpin>(wr: &mut W, b: &[u8]) -> std::io::Result<()> {
    tokio::time::timeout(WRITE_TIMEOUT, wr.write_all(b)).await.map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))?
}

const CORS: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\n";

/// Binds and serves until the returned task is aborted; returns the bound address and the task.
pub async fn serve(feed: Arc<TipFeed>, cfg: SseConfig) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let l = TcpListener::bind(cfg.bind).await?;
    let addr = l.local_addr()?;
    let conns = Arc::new(tokio::sync::Semaphore::new(feed.cfg.max_total + EXTRA_CONNS));
    let acceptor = cfg.tls.as_ref().map(|t| t.acceptor());
    let h = tokio::spawn(async move {
        let mut check = tokio::time::interval(cfg.tls_check.max(Duration::from_millis(10)));
        check.tick().await;
        loop {
            let (sock, peer) = tokio::select! {
                _ = check.tick() => {
                    if let Some(t) = &cfg.tls {
                        match t.reload_if_changed() {
                            Ok(true) => eprintln!("sm64coin-lc: TLS certificate reloaded"),
                            Ok(false) => {}
                            Err(e) => eprintln!("sm64coin-lc: TLS certificate reload failed (keeping the old one): {e}"),
                        }
                    }
                    continue;
                }
                r = l.accept() => match r {
                    Ok(x) => x,
                    Err(_) => {
                        // out of file descriptors and the like: accept fails at once again, so do not spin on it
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                },
            };
            // over the connection cap: close at once (the browser retries later by itself)
            let Ok(permit) = conns.clone().try_acquire_owned() else { continue };
            let (feed, cfg, acceptor) = (feed.clone(), cfg.clone(), acceptor.clone());
            tokio::spawn(async move {
                let _ = match acceptor {
                    None => handle(sock, peer, feed, cfg).await,
                    Some(a) => match tokio::time::timeout(HANDSHAKE_TIMEOUT, a.accept(sock)).await {
                        Ok(Ok(tls)) => handle(tls, peer, feed, cfg).await,
                        _ => Ok(()),
                    },
                };
                drop(permit);
            });
        }
    });
    Ok((addr, h))
}

async fn handle<S: AsyncRead + AsyncWrite + Unpin>(mut sock: S, peer: SocketAddr, feed: Arc<TipFeed>, cfg: SseConfig) -> std::io::Result<()> {
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
    let ip = if cfg.trust_forwarded && cfg.tls.is_none() {
        text.lines()
            .find_map(|l| l.to_ascii_lowercase().strip_prefix("x-forwarded-for:").map(|v| v.to_string()))
            .and_then(|v| v.split(',').next_back().and_then(|x| x.trim().parse::<IpAddr>().ok()))
            .unwrap_or(peer.ip())
    } else {
        peer.ip()
    };
    let Some(_sub) = feed.acquire(&ip_key(ip), cfg.max_per_ip) else {
        return sock.write_all(format!("HTTP/1.1 429 Too Many Requests\r\n{CORS}Retry-After: 30\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
    };
    // The browser reconnects by itself after `retry` ms: spread over 4-10 s, so a restart does not bring every page back at once.
    let retry = 4000 + (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0) ^ (peer.port() as u32 * 7919)) % 6000;
    let (mut rd, mut wr) = tokio::io::split(sock);
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
