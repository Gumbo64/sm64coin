//! The tip stream over HTTPS (the node serves it straight to the public): handshake with a self-signed certificate, the stream,
//! a certificate swapped on disk reaching new connections without a restart (and a broken swap being refused), and the per-IP caps
//! counting the TCP peer (a forged X-Forwarded-For buys nothing). Run with `cargo test --features tip-server --test tls`.
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use sm64coin_lc::exec::{UpError, Upstream};
use sm64coin_lc::sse::{SseConfig, TlsReloader, serve};
use sm64coin_lc::tipfeed::{TipFeed, TipFeedConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct FakeUp;

#[async_trait::async_trait]
impl Upstream for FakeUp {
    async fn json_rpc(&self, method: &str, _p: Value, _t: Duration) -> Result<Value, UpError> {
        let r = match method {
            "get_info" => json!({"height": 5, "top_block_hash": hex::encode([9u8; 32]), "cumulative_difficulty": 42, "difficulty": 1000}),
            "get_last_block_header" => json!({"block_header": {"hash": hex::encode([9u8; 32]), "height": 4, "timestamp": 1_700_000_000u64}}),
            _ => json!({}),
        };
        Ok(json!({"result": r}))
    }
    async fn post(&self, _: &str, _: &str, b: Vec<u8>, _: Duration) -> Result<Vec<u8>, UpError> {
        Ok(b)
    }
}

struct Pair {
    cert_der: Vec<u8>,
    cert_pem: String,
    key_pem: String,
}

fn make_cert() -> Pair {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    Pair { cert_der: ck.cert.der().to_vec(), cert_pem: ck.cert.pem(), key_pem: ck.key_pair.serialize_pem() }
}

fn write(dir: &std::path::Path, p: &Pair) -> (std::path::PathBuf, std::path::PathBuf) {
    let (c, k) = (dir.join("fullchain.pem"), dir.join("privkey.pem"));
    std::fs::write(&c, &p.cert_pem).unwrap();
    std::fs::write(&k, &p.key_pem).unwrap();
    (c, k)
}

fn tempdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("sm64coin-lc-tls-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

type Tls = tokio_rustls::client::TlsStream<tokio::net::TcpStream>;

/// Connects trusting only `root`; returns the stream and the certificate the server presented.
async fn connect(addr: std::net::SocketAddr, root: &[u8]) -> std::io::Result<(Tls, Vec<u8>)> {
    let mut store = rustls::RootCertStore::empty();
    store.add(rustls::pki_types::CertificateDer::from(root.to_vec())).unwrap();
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(store)
        .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(addr).await?;
    let s = tokio_rustls::TlsConnector::from(Arc::new(cfg)).connect("localhost".try_into().unwrap(), tcp).await?;
    let seen = s.get_ref().1.peer_certificates().unwrap()[0].as_ref().to_vec();
    Ok((s, seen))
}

async fn read_until(s: &mut Tls, needle: &str) -> String {
    let (mut total, mut buf) = (String::new(), [0u8; 2048]);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !total.contains(needle) {
            let n = s.read(&mut buf).await.unwrap();
            assert!(n > 0, "closed early: {total}");
            total.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {needle:?} in {total:?}"));
    total
}

async fn start(tls: Arc<TlsReloader>, max_per_ip: usize, trust_forwarded: bool, check: Duration) -> (Arc<TipFeed>, std::net::SocketAddr) {
    let feed = TipFeed::new(Arc::new(FakeUp), TipFeedConfig { poll: Duration::from_millis(100), idle_poll: Duration::from_millis(100), keepalive: Duration::from_millis(300), max_total: 100 });
    feed.refresh().await;
    tokio::spawn(feed.clone().run(|| false));
    let cfg = SseConfig { max_per_ip, trust_forwarded, tls: Some(tls), tls_check: check, ..Default::default() };
    let (addr, task) = serve(feed.clone(), cfg).await.unwrap();
    std::mem::forget(task);
    (feed, addr)
}

const REQ: &str = "GET /lc/tip HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n\r\n";

#[tokio::test(flavor = "multi_thread")]
async fn handshake_stream_and_cors_over_https() {
    let dir = tempdir("stream");
    let p = make_cert();
    let (c, k) = write(&dir, &p);
    let (_feed, addr) = start(TlsReloader::load(&c, &k).unwrap(), 4, false, Duration::from_secs(300)).await;
    let (mut s, seen) = connect(addr, &p.cert_der).await.expect("handshake");
    assert_eq!(seen, p.cert_der);
    s.write_all(REQ.as_bytes()).await.unwrap();
    let t = read_until(&mut s, "data: {").await;
    assert!(t.starts_with("HTTP/1.1 200 OK") && t.contains("Content-Type: text/event-stream") && t.contains("Access-Control-Allow-Origin: *"), "{t}");
    assert!(t.contains("\"height\":4"), "{t}");
    read_until(&mut s, "event: ka\ndata: ").await;
    // plain HTTP to the TLS port gets nothing usable, and a client that does not trust the certificate cannot connect
    let mut plain = tokio::net::TcpStream::connect(addr).await.unwrap();
    plain.write_all(REQ.as_bytes()).await.unwrap();
    let mut b = [0u8; 64];
    let r = tokio::time::timeout(Duration::from_secs(5), plain.read(&mut b)).await.expect("closed");
    assert!(!matches!(r, Ok(n) if b[..n].starts_with(b"HTTP/1.1 200")));
    assert!(connect(addr, &make_cert().cert_der).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renewed_certificate_is_picked_up_without_a_restart() {
    let dir = tempdir("reload");
    let old = make_cert();
    let (c, k) = write(&dir, &old);
    let rl = TlsReloader::load(&c, &k).unwrap();
    let (_feed, addr) = start(rl.clone(), 4, false, Duration::from_millis(100)).await;
    let (mut before, seen) = connect(addr, &old.cert_der).await.unwrap();
    assert_eq!(seen, old.cert_der);
    before.write_all(REQ.as_bytes()).await.unwrap();
    read_until(&mut before, "data: {").await;
    // a half-written renewal (new certificate, old key): refused, the old pair stays
    let new = make_cert();
    std::fs::write(&c, &new.cert_pem).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(rl.reloads(), 0);
    assert_eq!(connect(addr, &old.cert_der).await.unwrap().1, old.cert_der);
    // the key lands: the new pair is served to new connections within a check or two
    std::fs::write(&k, &new.key_pem).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while rl.reloads() == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await
        }
    })
    .await
    .expect("reloaded");
    let (mut after, seen) = connect(addr, &new.cert_der).await.expect("the new certificate is trusted-able");
    assert_eq!(seen, new.cert_der);
    after.write_all(REQ.as_bytes()).await.unwrap();
    read_until(&mut after, "data: {").await;
    // the stream opened before the renewal keeps running
    read_until(&mut before, "event: ka\ndata: ").await;
    // an explicit reload (SIGHUP) works too
    rl.reload().unwrap();
    assert_eq!(rl.reloads(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn caps_count_the_real_peer_not_a_forwarded_header() {
    let dir = tempdir("caps");
    let p = make_cert();
    let (c, k) = write(&dir, &p);
    // even with trust_forwarded set (a misconfiguration), a TLS listener is reached directly and ignores the header
    let (feed, addr) = start(TlsReloader::load(&c, &k).unwrap(), 2, true, Duration::from_secs(300)).await;
    let mut held = vec![];
    for i in 0..2 {
        let (mut s, _) = connect(addr, &p.cert_der).await.unwrap();
        s.write_all(format!("GET /lc/tip HTTP/1.1\r\nHost: localhost\r\nX-Forwarded-For: 203.0.113.{i}\r\n\r\n").as_bytes()).await.unwrap();
        read_until(&mut s, "data: {").await;
        held.push(s);
    }
    assert_eq!(feed.subscribers(), 2);
    let (mut s, _) = connect(addr, &p.cert_der).await.unwrap();
    s.write_all(b"GET /lc/tip HTTP/1.1\r\nHost: localhost\r\nX-Forwarded-For: 203.0.113.99\r\n\r\n").await.unwrap();
    let t = read_until(&mut s, "429").await;
    assert!(t.contains("Retry-After"), "{t}");
}

#[test]
fn bad_files_are_refused_at_load() {
    let dir = tempdir("bad");
    let p = make_cert();
    let (c, k) = write(&dir, &p);
    assert!(TlsReloader::load(dir.join("nope.pem"), &k).is_err());
    std::fs::write(&k, make_cert().key_pem).unwrap();
    assert!(TlsReloader::load(&c, &k).unwrap_err().contains("match"));
    std::fs::write(&c, "not pem").unwrap();
    assert!(TlsReloader::load(&c, &k).is_err());
}
