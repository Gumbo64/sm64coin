//! Tip push over plain HTTP (Server-Sent Events): the shared watcher, the SSE listener, the HTTP client
//! stream, `Client::watch_tip` over HTTP nodes, and the fall-back when a node has no such endpoint.
//! Run with `cargo test --features tip-server --test sse`.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use sm64coin_lc::client::*;
use sm64coin_lc::exec::{UpError, Upstream};
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::nodes::{NodeAddr, NodeList, Source};
use sm64coin_lc::sse::{SseConfig, serve};
use sm64coin_lc::tipfeed::{TipFeed, TipFeedConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Default)]
struct FakeUp {
    calls: AtomicUsize,
    height: Mutex<u64>,
    hash: Mutex<u8>,
}

#[async_trait::async_trait]
impl Upstream for FakeUp {
    async fn json_rpc(&self, method: &str, _p: Value, _t: Duration) -> Result<Value, UpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (h, hb) = (*self.height.lock().unwrap(), *self.hash.lock().unwrap());
        let r = match method {
            "get_info" => json!({"height": h + 1, "top_block_hash": hex::encode([hb; 32]), "cumulative_difficulty": 42, "difficulty": 1000}),
            "get_last_block_header" => json!({"block_header": {"hash": hex::encode([hb; 32]), "height": h, "timestamp": 1_700_000_000u64 + h}}),
            _ => json!({}),
        };
        Ok(json!({"result": r}))
    }
    async fn post(&self, _: &str, _: &str, b: Vec<u8>, _: Duration) -> Result<Vec<u8>, UpError> {
        Ok(b)
    }
}

fn mine(up: &FakeUp, hash: u8) {
    *up.height.lock().unwrap() += 1;
    *up.hash.lock().unwrap() = hash;
}

async fn start(max_per_ip: usize) -> (Arc<FakeUp>, Arc<TipFeed>, std::net::SocketAddr, tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
    let up = Arc::new(FakeUp::default());
    *up.height.lock().unwrap() = 4;
    *up.hash.lock().unwrap() = 9;
    let feed = TipFeed::new(up.clone(), TipFeedConfig { poll: Duration::from_millis(100), idle_poll: Duration::from_millis(100), keepalive: Duration::from_millis(300), max_total: 100 });
    feed.refresh().await;
    let poller = tokio::spawn(feed.clone().run(|| false));
    let (addr, task) = serve(feed.clone(), SseConfig { max_per_ip, ..Default::default() }).await.unwrap();
    (up, feed, addr, poller, task)
}

async fn raw_get(addr: std::net::SocketAddr, req: &str) -> tokio::net::TcpStream {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    s
}

async fn read_until(s: &mut tokio::net::TcpStream, needle: &str, total: &mut String) {
    let mut buf = [0u8; 2048];
    tokio::time::timeout(Duration::from_secs(5), async {
        while !total.contains(needle) {
            let n = s.read(&mut buf).await.unwrap();
            assert!(n > 0, "closed early: {total}");
            total.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {needle:?} in {total:?}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn sse_headers_events_keepalive_caps_and_other_paths() {
    let (up, feed, addr, _p, _t) = start(2).await;
    let req = "GET /lc/tip HTTP/1.1\r\nHost: x\r\nAccept: text/event-stream\r\n\r\n";
    let mut a = raw_get(addr, req).await;
    let mut ta = String::new();
    read_until(&mut a, "data: {", &mut ta).await;
    assert!(ta.starts_with("HTTP/1.1 200 OK"), "{ta}");
    assert!(ta.contains("Content-Type: text/event-stream") && ta.contains("Access-Control-Allow-Origin: *") && ta.contains("Cache-Control: no-cache"), "{ta}");
    assert!(ta.contains("\"height\":4") && ta.contains(&hex::encode([9u8; 32])) && ta.contains("\"difficulty\":\"1000\""), "{ta}");
    // a new block reaches every subscriber, within a poll or two
    let mut b = raw_get(addr, req).await;
    let mut tb = String::new();
    read_until(&mut b, "data: {", &mut tb).await;
    assert_eq!(feed.subscribers(), 2);
    let t0 = std::time::Instant::now();
    mine(&up, 10);
    let want = hex::encode([10u8; 32]);
    read_until(&mut a, &want, &mut ta).await;
    read_until(&mut b, &want, &mut tb).await;
    assert!(t0.elapsed() < Duration::from_millis(1500), "{:?}", t0.elapsed());
    // keepalive events while nothing changes, carrying the server's clock; the record's server_time is the clock at sending
    read_until(&mut a, "event: ka\ndata: ", &mut ta).await;
    let ka_at = ta.rfind("event: ka\ndata: ").unwrap() + "event: ka\ndata: ".len();
    read_until(&mut a, "event: ka\ndata: 1", &mut ta).await; // whole line arrives with the event
    let clock: u64 = ta[ka_at..].split('\n').next().unwrap().trim().parse().unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    assert!(clock + 2 >= now && clock <= now + 2, "ka clock {clock} vs {now}");
    assert!(ta.contains(&format!("\"server_time\":{}", now)) || ta.contains(&format!("\"server_time\":{}", now - 1)) || ta.contains(&format!("\"server_time\":{}", now - 2)), "{ta}");
    // the per-IP cap: a third from the same address is 429
    let mut c = raw_get(addr, req).await;
    let mut tc = String::new();
    read_until(&mut c, "429", &mut tc).await;
    // other paths and methods
    let mut d = raw_get(addr, "GET /json_rpc HTTP/1.1\r\n\r\n").await;
    let mut td = String::new();
    read_until(&mut d, "404", &mut td).await;
    let mut e = raw_get(addr, "OPTIONS /lc/tip HTTP/1.1\r\nOrigin: https://x\r\n\r\n").await;
    let mut te = String::new();
    read_until(&mut e, "204", &mut te).await;
    assert!(te.contains("Access-Control-Allow-Origin: *"));
    // one node poll for everybody: ~10 per second at most while subscribed, not per subscriber
    // a closed connection frees its slot
    drop(a);
    tokio::time::timeout(Duration::from_secs(5), async { while feed.subscribers() > 1 { tokio::time::sleep(Duration::from_millis(50)).await } }).await.expect("slot freed");
    let mut f = raw_get(addr, req).await;
    let mut tf = String::new();
    read_until(&mut f, "200 OK", &mut tf).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_http_client_follows_the_tip_and_falls_back_without_the_endpoint() {
    let (up, _feed, addr, _p, _t) = start(8).await;
    let base = format!("http://{addr}");
    let t = UpstreamTransport(HttpUpstream::new(&base).unwrap());
    let mut st = t.open_tip_stream().await.expect("SSE");
    let first = tokio::time::timeout(Duration::from_secs(5), st.next()).await.unwrap().unwrap().unwrap();
    assert_eq!((first.height, first.hash, first.difficulty, first.timestamp), (4, [9; 32], 1000, 1_700_000_004));
    mine(&up, 11);
    let second = tokio::time::timeout(Duration::from_secs(5), st.next()).await.unwrap().unwrap().unwrap();
    assert_eq!((second.height, second.hash), (5, [11; 32]));
    // watch_tip over an HTTP node list
    struct C;
    impl Connector for C {
        fn transport(&self, a: &NodeAddr) -> Option<Arc<dyn Transport>> {
            let NodeAddr::Http { url, .. } = a;
            Some(Arc::new(UpstreamTransport(HttpUpstream::new(url).unwrap())))
        }
    }
    let mut list = NodeList::new();
    list.add(NodeAddr::http(base.clone()), Source::Bootstrap, 0);
    let client = Arc::new(Client::new(list, Arc::new(C), Arc::new(SystemEnv)));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let cl = client.clone();
    let h = tokio::spawn(async move { cl.watch_tip(move |_, t| { let _ = tx.send(t); }).await });
    let a = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.unwrap().unwrap();
    assert_eq!(a.height, 5);
    mine(&up, 12);
    let b = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
    assert_eq!((b.height, b.hash), (6, [12; 32]));
    h.abort();
    // a node that serves no /lc/tip (a plain JSON-RPC node): Unsupported, and the watch tells the caller to poll
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let plain = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let mut b = [0u8; 1024];
                let _ = s.read(&mut b).await;
                let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            });
        }
    });
    let t2 = UpstreamTransport(HttpUpstream::new(&format!("http://{plain}")).unwrap());
    assert!(matches!(t2.open_tip_stream().await, Err(TransportError::Unsupported)));
    let mut list = NodeList::new();
    list.add(NodeAddr::http(format!("http://{plain}")), Source::Bootstrap, 0);
    let client = Client::new(list, Arc::new(C), Arc::new(SystemEnv));
    let end = tokio::time::timeout(Duration::from_secs(10), client.watch_tip(|_, _| panic!("a plain node pushes nothing"))).await.expect("returns");
    assert!(matches!(end, WatchEnd::NoPush));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_advertised_tip_url_is_used_instead_of_the_nodes_own_and_a_dead_one_does_not_count_against_the_node() {
    let (_up, _feed, addr, _p, _t) = start(8).await;
    // the node's RPC address serves no stream (nothing listens there); its advertised tip URL does
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let t = UpstreamTransport(HttpUpstream::new(&format!("http://{dead}")).unwrap().with_tip_url(Some(format!("http://{addr}/lc/tip"))));
    let mut st = t.open_tip_stream().await.expect("the advertised tip URL");
    let first = tokio::time::timeout(Duration::from_secs(5), st.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(first.height, 4);
    // an advertised tip URL that cannot be reached: not supported (poll), not an RPC failure
    let t = UpstreamTransport(HttpUpstream::new(&format!("http://{addr}")).unwrap().with_tip_url(Some(format!("http://{dead}/lc/tip"))));
    assert!(matches!(t.open_tip_stream().await, Err(TransportError::Unsupported)));
}
