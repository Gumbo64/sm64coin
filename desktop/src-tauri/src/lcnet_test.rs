//! Failover of the app's light-client network, against ISOLATED nodes only: two `sm64coind --offline --sm64-solo` on fresh data
//! dirs and loopback ports, plus dead nodes (a closed port) and, for the tip push, a tip service (Server-Sent Events) in front of
//! a node. Nothing here touches the real network or chain. Needs the daemon and the ROM (the unpacked release bundle in
//! ~/apps/sm64coin-*, or SM64COIND and SM64COIN_ROM); skips, and says so, without them.
//!
//!   cargo test --lib lcnet_test -- --nocapture
use crate::lcnet::{MAINNET_GENESIS, Net};
use crate::remote::Remote;
use crate::walletproxy;
use serde_json::{Value, json};
use sm64coin_lc::exec::{self, Upstream};
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::nodes::{NodeAddr, Source};
use sm64coin_lc::sse::{SseConfig, serve};
use sm64coin_lc::tipfeed::{TipFeed, TipFeedConfig};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

struct Daemon {
    child: Child,
    rpc: u16,
    dir: PathBuf,
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill(); // only the child this test started
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn bundle_file(name: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let mut dirs: Vec<_> = std::fs::read_dir(format!("{home}/apps")).ok()?.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("sm64coin-"))).collect();
    dirs.sort();
    dirs.into_iter().rev().map(|d| d.join(name)).find(|p| p.is_file())
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let t = to.join(e.file_name());
        if e.file_type()?.is_dir() { copy_dir(&e.path(), &t)? } else { std::fs::copy(e.path(), &t).map(|_| ())? }
    }
    Ok(())
}

/// A pre-mined isolated chain for the wallet flow (`SM64COIN_FLOW_FIXTURE`: a directory with `node/`, the stopped daemon's
/// data dir of an `--offline --sm64-solo` chain, and `wallets/` holding w1 (the miner's payee) and w2). Never the real chain.
fn flow_fixture() -> Option<PathBuf> {
    std::env::var_os("SM64COIN_FLOW_FIXTURE").map(PathBuf::from).filter(|d| d.join("node").is_dir())
}

fn start_daemon(tag: &str) -> Option<Daemon> {
    let bin = std::env::var_os("SM64COIND").map(PathBuf::from).or_else(|| bundle_file("sm64coind"))?;
    let rom = std::env::var_os("SM64COIN_ROM").map(PathBuf::from).or_else(|| bundle_file("baserom.us.z64"))?;
    let dir = std::env::temp_dir().join(format!("lcnet-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    if tag == "flow" {
        if let Some(fx) = flow_fixture() {
            // a copy: the fixture itself stays as it was
            copy_dir(&fx.join("node"), &dir.join("node")).expect("copy the chain fixture");
        }
    }
    let (rpc, p2p, zmq) = (free_port(), free_port(), free_port());
    let child = Command::new(bin)
        .args(["--offline", "--sm64-solo", "--no-igd", "--non-interactive", "--p2p-bind-ip", "127.0.0.1", "--rpc-bind-ip", "127.0.0.1"])
        .args(["--rpc-bind-port", &rpc.to_string(), "--p2p-bind-port", &p2p.to_string(), "--zmq-rpc-bind-port", &zmq.to_string()])
        .arg("--data-dir")
        .arg(dir.join("node"))
        .arg("--sm64-rom")
        .arg(rom)
        .arg("--log-file")
        .arg(dir.join("d.log"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    Some(Daemon { child, rpc, dir })
}

fn http_of(d: &Daemon) -> NodeAddr {
    NodeAddr::http(format!("http://127.0.0.1:{}", d.rpc))
}

/// A tip service (the shared watcher and Server-Sent Events, as `sm64coin-lc serve` runs it) in front of `d`, on a loopback port; the
/// node address a client lists to follow the tip by push. Lives as long as the async runtime.
async fn tip_service(d: &Daemon) -> NodeAddr {
    let up: Arc<dyn Upstream> = Arc::new(HttpUpstream::new(&format!("http://127.0.0.1:{}", d.rpc)).unwrap());
    let feed = TipFeed::new(up, TipFeedConfig::default());
    feed.refresh().await;
    tokio::spawn(feed.clone().run(|| false));
    let (addr, task) = serve(feed, SseConfig::default()).await.unwrap();
    std::mem::forget(task);
    NodeAddr::http(format!("http://{addr}"))
}

fn dead_http() -> NodeAddr {
    NodeAddr::http(format!("http://127.0.0.1:{}", free_port())) // nothing listens
}

fn net_with(list: &[(NodeAddr, Source)]) -> Arc<Net> {
    let boot: Vec<NodeAddr> = list.iter().filter(|(_, s)| *s == Source::Bootstrap).map(|(a, _)| a.clone()).collect();
    let net = Net::new(&boot, None, None);
    let mut n = Arc::try_unwrap(net).ok().expect("fresh");
    n.read_timeout = Duration::from_secs(4);
    n.bulk_timeout = Duration::from_secs(8);
    Arc::new(n)
}

/// POST to the proxy; (status, body).
fn post(url: &str, path: &str, ct: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let r = ureq::post(&format!("{url}{path}")).timeout(Duration::from_secs(30)).set("content-type", ct).send_bytes(body);
    let resp = match r {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => panic!("proxy unreachable: {e}"),
    };
    let code = resp.status();
    let mut data = Vec::new();
    std::io::Read::read_to_end(&mut resp.into_reader(), &mut data).unwrap();
    (code, data)
}

fn jrpc(url: &str, method: &str, params: Value) -> Value {
    let (code, body) = post(url, "/json_rpc", "application/json", json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}).to_string().as_bytes());
    assert_eq!(code, 200, "{method}: {}", String::from_utf8_lossy(&body));
    serde_json::from_slice(&body).unwrap()
}

/// An empty epee "portable storage" section: a valid request body for the wallet's binary calls that take no arguments.
const EMPTY_EPEE: [u8; 10] = [0x01, 0x11, 0x01, 0x01, 0x01, 0x01, 0x02, 0x01, 0x01, 0x00];

#[test]
fn http_failover_through_remote_and_the_wallet_proxy() {
    let (Some(a), Some(b)) = (start_daemon("a"), start_daemon("b")) else {
        eprintln!("SKIPPED: set SM64COIND and SM64COIN_ROM (or unpack a release bundle in ~/apps)");
        return;
    };
    // wait for both isolated nodes and take the genesis hash 
    let t0 = std::time::Instant::now();
    let mut network = None;
    for d in [&a, &b] {
        let up = HttpUpstream::new(&format!("http://127.0.0.1:{}", d.rpc)).unwrap();
        let g = loop {
            match tauri::async_runtime::block_on(exec::genesis_hash(&up, Duration::from_secs(2))) {
                Ok(g) => break g,
                Err(_) => {
                    assert!(t0.elapsed() < Duration::from_secs(90), "the isolated node did not answer");
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        };
        assert!(network.is_none_or(|n| n == g), "two solo nodes share the genesis block");
        network = Some(g);
    }
    let network = network.unwrap();
    let mainnet = hex::decode(MAINNET_GENESIS).unwrap();
    eprintln!("isolated nodes' genesis {} (mainnet {})", hex::encode(network), hex::encode(&mainnet));

    // ---------------------------------------------------------------- HTTP: a dead node, then a live one
    {
        let net = net_with(&[(dead_http(), Source::Bootstrap), (http_of(&b), Source::Bootstrap)]);
        let proxy = walletproxy::start(net.clone(), vec![]).unwrap();
        let url = proxy.url();
        for i in 0..6 {
            let v = jrpc(&url, "get_info", json!({}));
            assert_eq!(v["id"], 7, "the caller's id comes back");
            assert_eq!(v["result"]["height"], 1, "call {i}: a fresh chain holds genesis only");
            let v = jrpc(&url, "get_fee_estimate", json!({})); // has a protocol message: Client::read over the HTTP transport
            assert!(v["result"].get("fee").is_some(), "{v}");
        }
        let (code, body) = post(&url, "/gethashes.bin", "application/octet-stream", &EMPTY_EPEE);
        assert_eq!(code, 200, "{}", String::from_utf8_lossy(&body));
        assert_eq!(&body[..4], &[0x01, 0x11, 0x01, 0x01], "an epee answer");
        let st = net.status();
        let used = st.used.expect("a node was used");
        assert!(used.node.ends_with(&format!(":{}", b.rpc)) && used.transport == "http", "{used:?}");
        assert_eq!(st.proxy.as_deref(), Some(url.as_str()));
        let dead = st.nodes.iter().find(|n| !n.key.ends_with(&format!(":{}", b.rpc))).unwrap();
        let live = st.nodes.iter().find(|n| n.key.ends_with(&format!(":{}", b.rpc))).unwrap();
        assert!(dead.ok_rate <= live.ok_rate, "the dead node was not scored above the live one (picks are weighted-random, so it may never have been tried): {dead:?} {live:?}");

        // what the proxy refuses
        assert_eq!(post(&url, "/json_rpc", "application/json", br#"{"jsonrpc":"2.0","id":1,"method":"stop_daemon"}"#).0, 403);
        assert_eq!(post(&url, "/stop_daemon", "application/json", b"{}").0, 403);
        assert_eq!(post(&url, "/get_public_nodes", "application/json", b"{}").0, 403);
        let foreign = ureq::post(&format!("{url}/json_rpc")).set("origin", "https://evil.example").send_string("{}");
        assert!(matches!(foreign, Err(ureq::Error::Status(403, _))), "a foreign Origin is refused");
        let app = ureq::request("OPTIONS", &format!("{url}/json_rpc")).set("origin", "tauri://localhost").set("access-control-request-method", "POST").call().unwrap();
        assert_eq!(app.header("access-control-allow-origin"), Some("tauri://localhost"));

        // the native miner's own client (Android Campaign) over the proxy: the daemon it is given
        let d = sm64coin_miner::rpc::Daemon::new(&url);
        assert_eq!(d.height().unwrap(), 1);
        assert!(d.top_hash().unwrap().len() == 64);
        drop(proxy);
    }

    // ---------------------------------------------------------------- remote_rpc: the public node's calls go through the network
    {
        let net = net_with(&[(dead_http(), Source::Bootstrap), (http_of(&a), Source::Bootstrap)]);
        let remote = Remote::with_net(None, None, net.clone());
        // the page names the compiled-in public node; Rust answers from whichever node of the list works
        let public = crate::remote::public_node().expect("the build has a public node");
        for _ in 0..4 {
            let v = tauri::async_runtime::block_on(remote.rpc(&public, "get_info", json!({}))).unwrap();
            assert_eq!(v["result"]["height"], 1);
        }
        assert_eq!(tauri::async_runtime::block_on(remote.rpc("https://evil.example", "get_info", json!({}))).unwrap_err().code, "forbidden");
        assert_eq!(tauri::async_runtime::block_on(remote.rpc(&public, "stop_daemon", json!({}))).unwrap_err().code, "rpc");
        assert!(net.status().used.unwrap().node.ends_with(&format!(":{}", a.rpc)));
    }

    // ---------------------------------------------------------------- every kind of call, over a dead node and a live one
    {
        let net = net_with(&[(dead_http(), Source::Bootstrap), (http_of(&a), Source::Bootstrap)]);
        // an isolated node knows no public nodes: discovery finds nothing and does no harm
        assert_eq!(tauri::async_runtime::block_on(net.discover()), 0);
        let proxy = walletproxy::start(net.clone(), vec![]).unwrap();
        let url = proxy.url();
        let mut ok = 0;
        for _ in 0..8 {
            // get_fee_estimate and the wallet's binary calls have protocol messages: they fail over between nodes
            let v = jrpc(&url, "get_fee_estimate", json!({}));
            assert!(v["result"].get("fee").is_some(), "{v}");
            let (code, body) = post(&url, "/gethashes.bin", "application/octet-stream", &EMPTY_EPEE);
            assert_eq!(code, 200, "{}", String::from_utf8_lossy(&body));
            assert_eq!(&body[..4], &[0x01, 0x11, 0x01, 0x01]);
            ok += 1;
        }
        assert_eq!(ok, 8);
        let used = net.status().used.unwrap();
        assert_eq!(used.transport, "http", "{used:?}");
        // the headers of the genesis block
        let v = jrpc(&url, "get_block_headers_range", json!({"start_height": 0, "end_height": 0}));
        assert!(v["result"]["headers"].is_array(), "{v}");
        // calls without a typed message go over HTTP too (Request::JsonRpc): the node's own answer, not one made from its tip
        let v = jrpc(&url, "get_info", json!({}));
        assert!(v["result"]["height"] == 1 && v["result"].get("light").is_none() && v["result"].get("nettype").is_some(), "{v}");
        assert_eq!(net.status().used.unwrap().transport, "http");
        // the wallet's /getheight likewise
        let (code, body) = post(&url, "/getheight", "application/json", b"{}");
        assert_eq!(code, 200);
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["height"], 1);
        // the miner's template calls (sm64 extras) and the proof-of-play state
        let v = jrpc(&url, "get_sm64_state", json!({}));
        assert!(v.get("result").is_some() || v["error"]["message"].is_string(), "the node's own answer or its own error: {v}");
        let v = jrpc(&url, "get_block", json!({"height": 0}));
        assert!(v["result"]["blob"].is_string(), "{v}");
        // wallet paths: get_transactions, is_key_image_spent, get_o_indexes.bin, pool hashes, get_height, get_info
        let (code, body) = post(&url, "/get_transactions", "application/json", br#"{"txs_hashes":[],"decode_as_json":false}"#);
        assert_eq!(code, 200, "{}", String::from_utf8_lossy(&body));
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["status"], "OK");
        let (code, body) = post(&url, "/is_key_image_spent", "application/json", br#"{"key_images":["0000000000000000000000000000000000000000000000000000000000000000"]}"#);
        assert_eq!(code, 200, "{}", String::from_utf8_lossy(&body));
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["spent_status"], json!([0]));
        let (code, body) = post(&url, "/get_transaction_pool_hashes.bin", "application/json", b"{}"); // wallet2 sends this one as JSON
        assert_eq!(code, 200, "{}", String::from_utf8_lossy(&body));
        let (code, body) = post(&url, "/get_height", "application/json", b"{}");
        assert_eq!(code, 200, "{}", String::from_utf8_lossy(&body));
        for m in ["get_version", "hard_fork_info", "get_output_histogram", "get_txpool_backlog", "getblockheadersrange"] {
            let p = if m == "getblockheadersrange" { json!({"start_height": 0, "end_height": 0}) } else { json!({}) };
            let v = jrpc(&url, m, p);
            assert!(v.get("result").is_some(), "{m}: {v}");
        }
        // the calls of a send: decoys, output indexes, spent check, broadcast (junk is rejected by the node, not lost)
        let (code, body) = post(&url, "/get_outs.bin", "application/octet-stream", &EMPTY_EPEE);
        assert_eq!(code, 200, "{}", String::from_utf8_lossy(&body));
        let (code, body) = post(&url, "/get_o_indexes.bin", "application/octet-stream", &EMPTY_EPEE);
        assert!(code == 200 || code == 502, "{code} {}", String::from_utf8_lossy(&body)); // the node's own verdict on an empty request
        let (code, body) = post(&url, "/send_raw_transaction", "application/json", br#"{"tx_as_hex":"00ff","do_not_relay":false}"#);
        assert_eq!(code, 200, "{}", String::from_utf8_lossy(&body));
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["status"], "Failed", "junk transaction");
        // still refused: unrestricted calls
        assert_eq!(post(&url, "/get_transaction_pool", "application/json", b"{}").0, 403);
        assert_eq!(post(&url, "/json_rpc", "application/json", br#"{"jsonrpc":"2.0","id":1,"method":"get_connections"}"#).0, 403);
        // submissions fan out and report a node's verdict: junk is rejected by the node, not lost
        let v = jrpc(&url, "submit_block", json!(["000102"]));
        assert!(v.get("error").is_some(), "junk block is rejected: {v}");
        drop(proxy);
    }

    // ---------------------------------------------------------------- everything at once
    {
        let net = net_with(&[(dead_http(), Source::Bootstrap), (dead_http(), Source::Bootstrap), (http_of(&b), Source::Bootstrap)]);
        let proxy = walletproxy::start(net.clone(), vec![]).unwrap();
        let url = proxy.url();
        for _ in 0..24 {
            let v = jrpc(&url, "get_info", json!({})); // HTTP only: the live http node
            assert_eq!(v["result"]["height"], 1);
            let v = jrpc(&url, "get_fee_estimate", json!({})); // any live node
            assert!(v["result"].get("fee").is_some());
        }
        let st = net.status();
        assert!(st.nodes.iter().filter(|n| n.ok_rate > 0.5).count() >= 1, "the live node is scored up: {:?}", st.nodes);
        assert!(st.nodes.iter().any(|n| n.banned) || st.nodes.iter().any(|n| n.ok_rate < 0.4), "the dead ones are scored down: {:?}", st.nodes);
        drop(proxy);
    }
}

/// The wallet-rpc sidecar (wallet2) syncs through the proxy, with a dead node in the list: what the desktop app does when its own
/// node is not running. Isolated node only; skips without the sidecar binary.
#[test]
fn wallet_rpc_sidecar_syncs_through_the_proxy() {
    let (Some(a), Some(wrpc)) = (start_daemon("w"), bundle_file("sm64coin-wallet-rpc")) else {
        eprintln!("SKIPPED: needs sm64coind, the ROM and sm64coin-wallet-rpc (an unpacked release bundle in ~/apps)");
        return;
    };
    let up = HttpUpstream::new(&format!("http://127.0.0.1:{}", a.rpc)).unwrap();
    let t0 = std::time::Instant::now();
    while tauri::async_runtime::block_on(exec::genesis_hash(&up, Duration::from_secs(2))).is_err() {
        assert!(t0.elapsed() < Duration::from_secs(90), "the isolated node did not answer");
        std::thread::sleep(Duration::from_millis(300));
    }
    let net = net_with(&[(dead_http(), Source::Bootstrap), (http_of(&a), Source::Bootstrap)]);
    let proxy = walletproxy::start(net.clone(), vec![]).unwrap();
    struct Child2(Child);
    impl Drop for Child2 {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let wdir = a.dir.join("wallets");
    std::fs::create_dir_all(&wdir).unwrap();
    let wport = free_port();
    let _w = Child2(
        Command::new(&wrpc)
            .args(["--daemon-address", &proxy.addr(), "--untrusted-daemon", "--rpc-bind-ip", "127.0.0.1", "--rpc-bind-port", &wport.to_string(), "--disable-rpc-login", "--kdf-rounds", "1", "--non-interactive", "--no-dns"])
            .arg("--wallet-dir")
            .arg(&wdir)
            .arg("--log-file")
            .arg(a.dir.join("w.log"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the wallet sidecar"),
    );
    let wurl = format!("http://127.0.0.1:{wport}");
    let t1 = std::time::Instant::now();
    loop {
        let r = ureq::post(&format!("{wurl}/json_rpc")).timeout(Duration::from_secs(3)).send_json(json!({"jsonrpc": "2.0", "id": 0, "method": "get_version"}));
        if r.is_ok() {
            break;
        }
        assert!(t1.elapsed() < Duration::from_secs(60), "the wallet sidecar did not start");
        std::thread::sleep(Duration::from_millis(300));
    }
    let w = |m: &str, p: Value| -> Value {
        let r = ureq::post(&format!("{wurl}/json_rpc")).timeout(Duration::from_secs(60)).send_json(json!({"jsonrpc": "2.0", "id": 0, "method": m, "params": p})).unwrap();
        r.into_json().unwrap()
    };
    let c = w("create_wallet", json!({"filename": "t", "password": "", "language": "English"}));
    assert!(c.get("error").is_none(), "{c}");
    let r = w("refresh", json!({}));
    assert!(r.get("error").is_none(), "wallet2 refreshes through the proxy: {r}");
    let h = w("get_height", json!({}));
    assert_eq!(h["result"]["height"], 1, "{h}");
    let b = w("get_balance", json!({}));
    assert!(b.get("error").is_none(), "{b}");
    let used = net.status().used.expect("the proxy served the wallet");
    assert!(used.node.ends_with(&format!(":{}", a.rpc)), "{used:?}");
}

/// A node that lies about the genesis block: it forwards everything to a real isolated node but serves another id for block 0.
struct Liar {
    server: Arc<tiny_http::Server>,
    addr: NodeAddr,
}
impl Drop for Liar {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

fn liar_for(real: &Daemon) -> Liar {
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
    let port = server.server_addr().to_ip().unwrap().port();
    let (s2, rpc) = (server.clone(), real.rpc);
    std::thread::spawn(move || {
        for mut req in s2.incoming_requests() {
            let mut body = Vec::new();
            let _ = std::io::Read::read_to_end(req.as_reader(), &mut body);
            let method = serde_json::from_slice::<Value>(&body).ok().and_then(|v| v["method"].as_str().map(String::from)).unwrap_or_default();
            let url = format!("http://127.0.0.1:{rpc}{}", req.url());
            let mut out = match ureq::post(&url).set("content-type", "application/json").send_bytes(&body) {
                Ok(r) => {
                    let mut d = Vec::new();
                    std::io::Read::read_to_end(&mut r.into_reader(), &mut d).unwrap();
                    d
                }
                Err(_) => b"{}".to_vec(),
            };
            if method == "get_block" {
                if let Ok(mut v) = serde_json::from_slice::<Value>(&out) {
                    if v["result"]["block_header"]["height"] == 0 {
                        v["result"]["block_header"]["hash"] = json!("11".repeat(32));
                        out = v.to_string().into_bytes();
                    }
                }
            }
            let _ = req.respond(tiny_http::Response::from_data(out).with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap()));
        }
    });
    Liar { server, addr: NodeAddr::http(format!("http://127.0.0.1:{port}")) }
}

fn wait_for(d: &Daemon) -> [u8; 32] {
    let up = HttpUpstream::new(&format!("http://127.0.0.1:{}", d.rpc)).unwrap();
    let t0 = std::time::Instant::now();
    loop {
        match tauri::async_runtime::block_on(exec::genesis_hash(&up, Duration::from_secs(2))) {
            Ok(g) => return g,
            Err(_) => {
                assert!(t0.elapsed() < Duration::from_secs(90), "the isolated node did not answer");
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
}

/// The verifier across nodes from the genesis checkpoint (the isolated chain is its own, so the anchor is its
/// genesis block, not the release's mainnet checkpoint): agreeing nodes, a node that lies about the anchor (banned, never used
/// again), nothing reachable (unverified).
#[test]
fn verification_follows_the_verified_branch_and_bans_liars() {
    let (Some(a), Some(b)) = (start_daemon("va"), start_daemon("vb")) else {
        eprintln!("SKIPPED: set SM64COIND and SM64COIN_ROM (or unpack a release bundle in ~/apps)");
        return;
    };
    let network = wait_for(&a);
    assert_eq!(wait_for(&b), network);
    let cp = sm64coin_lc::verify::Checkpoint::genesis(network);
    let liar = liar_for(&a);

    // nothing to ask: unverified, and reads are not restricted
    {
        let net = net_with(&[(dead_http(), Source::Bootstrap)]);
        assert_eq!(net.verification().state, "checking", "before the first run");
        assert!(tauri::async_runtime::block_on(net.verify_now(Some(cp.clone()))).is_err());
        let v = net.verification();
        assert_eq!(v.state, "unverified", "{v:?}");
        assert!(v.label.starts_with("unverified") && v.error.is_some());
    }
    // two honest nodes
    {
        let net = net_with(&[(http_of(&a), Source::Bootstrap), (http_of(&b), Source::Bootstrap)]);
        tauri::async_runtime::block_on(net.verify_now(Some(cp.clone()))).unwrap();
        let v = net.verification();
        assert_eq!((v.state, v.agree, v.label.as_str()), ("verified", 2, "verified (2 nodes agree)"), "{v:?}");
        assert!(v.banned.is_empty() && !v.quorum, "two nodes are below the quorum of three: {v:?}");
        assert_eq!(v.height, Some(0));
        assert_eq!(net.status().verify.state, "verified", "net_status carries it");
    }
    // an honest node, a node that lies about the anchor, a dead one, another honest one
    {
        let net = net_with(&[(http_of(&a), Source::Bootstrap), (liar.addr.clone(), Source::Bootstrap), (dead_http(), Source::Bootstrap), (http_of(&b), Source::Bootstrap)]);
        tauri::async_runtime::block_on(net.verify_now(Some(cp.clone()))).unwrap();
        let v = net.verification();
        assert_eq!((v.state, v.agree), ("conflict", 2), "{v:?}");
        assert_eq!(v.banned.len(), 1, "the liar is banned: {v:?}");
        assert!(v.label.starts_with("conflict"), "{}", v.label);
        let st = net.status();
        let liar_key = liar.addr.key();
        assert!(st.nodes.iter().find(|n| n.key == liar_key).unwrap().banned);
        // from now on the wallet proxy's and the miner's calls use only the nodes on the verified branch
        let proxy = walletproxy::start(net.clone(), vec![]).unwrap();
        let url = proxy.url();
        let mut used = std::collections::BTreeSet::new();
        for _ in 0..16 {
            assert_eq!(jrpc(&url, "get_info", json!({}))["result"]["height"], 1);
            let u = net.status().used.unwrap();
            assert!(u.node != liar_key && !u.node.contains(&format!(":{}", free_port())), "{u:?}");
            used.insert(u.node);
            jrpc(&url, "get_fee_estimate", json!({}));
            used.insert(net.status().used.unwrap().node);
        }
        assert!(used.iter().all(|n| n == &http_of(&a).key() || n == &http_of(&b).key()), "only verified nodes served: {used:?}");
        drop(proxy);
    }
}

/// The whole wallet flow through the proxy over HTTP (the one node, with a dead one in the list too): the native miner (as on Android: its daemon is the proxy) mines to test wallet 1 on an
/// isolated offline node, wallet-rpc (wallet2) syncs, then sends to test wallet 2, which receives it. Then the verifier checks the
/// mined chain (header layer; and on desktop the replay of the current cycle) and says "verified". Never touches the real chain.
///
/// Slow (the miner plays the game): SM64COIN_FLOW_BLOCKS (default 24) blocks, SM64COIN_TEST_SECS (default 3600) at most.
#[test]
fn wallet_send_flow_over_http_with_the_native_miner() {
    use crate::mobile_miner::MobileMiner;
    if std::env::var_os("SM64COIN_FLOW").is_none() {
        eprintln!("SKIPPED: the mining flow takes hours (the miner plays the game for every block, ring size 16 needs ~24 blocks); set SM64COIN_FLOW=1");
        return;
    }
    let (Some(a), Some(wrpc), Some(rom)) = (start_daemon("flow"), bundle_file("sm64coin-wallet-rpc"), std::env::var_os("SM64COIN_ROM").map(PathBuf::from).or_else(|| bundle_file("baserom.us.z64"))) else {
        eprintln!("SKIPPED: needs sm64coind, the ROM and sm64coin-wallet-rpc (an unpacked release bundle in ~/apps)");
        return;
    };
    let network = wait_for(&a);
    let net = net_with(&[(dead_http(), Source::Bootstrap), (http_of(&a), Source::Bootstrap)]);
    let proxy = walletproxy::start(net.clone(), vec![]).unwrap();
    let wdir = a.dir.join("wallets");
    std::fs::create_dir_all(&wdir).unwrap();
    let wport = free_port();
    struct Child2(Child);
    impl Drop for Child2 {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _w = Child2(
        Command::new(&wrpc)
            .args(["--daemon-address", &proxy.addr(), "--untrusted-daemon", "--rpc-bind-ip", "127.0.0.1", "--rpc-bind-port", &wport.to_string(), "--disable-rpc-login", "--kdf-rounds", "1", "--non-interactive", "--no-dns"])
            .arg("--wallet-dir")
            .arg(&wdir)
            .arg("--log-file")
            .arg(a.dir.join("w.log"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the wallet sidecar"),
    );
    let wurl = format!("http://127.0.0.1:{wport}");
    let t1 = std::time::Instant::now();
    while ureq::post(&format!("{wurl}/json_rpc")).timeout(Duration::from_secs(3)).send_json(json!({"jsonrpc": "2.0", "id": 0, "method": "get_version"})).is_err() {
        assert!(t1.elapsed() < Duration::from_secs(60), "the wallet sidecar did not start");
        std::thread::sleep(Duration::from_millis(300));
    }
    let w = |m: &str, p: Value| -> Value {
        let r = ureq::post(&format!("{wurl}/json_rpc")).timeout(Duration::from_secs(300)).send_json(json!({"jsonrpc": "2.0", "id": 0, "method": m, "params": p}));
        match r {
            Ok(r) => r.into_json().unwrap(),
            Err(ureq::Error::Status(_, r)) => r.into_json().unwrap(),
            Err(e) => panic!("{m}: {e}"),
        }
    };
    let ok = |v: Value| -> Value {
        assert!(v.get("error").is_none(), "{v}");
        v["result"].clone()
    };
    // with a fixture its wallets (w1 is the payee of the pre-mined blocks); else two new ones
    let fixture = flow_fixture();
    if let Some(fx) = &fixture {
        copy_dir(&fx.join("wallets"), &wdir).expect("copy the fixture's wallets");
    }
    let make = if fixture.is_some() { "open_wallet" } else { "create_wallet" };
    ok(w(make, json!({"filename": "w2", "password": "", "language": "English"})));
    let addr2 = ok(w("get_address", json!({})))["address"].as_str().unwrap().to_string();
    ok(w("close_wallet", json!({})));
    ok(w(make, json!({"filename": "w1", "password": "", "language": "English"})));
    let addr1 = ok(w("get_address", json!({})))["address"].as_str().unwrap().to_string();

    // the native miner, behind the proxy (so its template, state and submit calls all travel through it)
    let m = MobileMiner::new(a.dir.join("app"));
    {
        use base64::Engine;
        m.rom_put_b64(&base64::engine::general_purpose::STANDARD.encode(std::fs::read(&rom).unwrap())).unwrap();
    }
    let emit: crate::mobile_miner::Emit = Arc::new(|_, _| {});
    assert!(proxy.url().starts_with("http://127.0.0.1:"), "this test mines only through its own loopback proxy to its own isolated node");
    let want: u64 = std::env::var("SM64COIN_FLOW_BLOCKS").ok().and_then(|s| s.parse().ok()).unwrap_or(24);
    let limit = Duration::from_secs(std::env::var("SM64COIN_TEST_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(3600));
    let t0 = std::time::Instant::now();
    let node = HttpUpstream::new(&format!("http://127.0.0.1:{}", a.rpc)).unwrap();
    let height = || -> u64 { tauri::async_runtime::block_on(node.json_rpc("get_info", json!({}), Duration::from_secs(10))).ok().and_then(|v| v["result"]["height"].as_u64()).unwrap_or(0) };
    // a fixture that is already long enough needs no mining here
    let mine = height() < want;
    if mine {
        m.start(emit.clone(), &proxy.url(), &addr1, Some(4), false).expect("start the miner");
    }
    while height() < want && t0.elapsed() < limit {
        std::thread::sleep(Duration::from_secs(10));
    }
    let h = height();
    eprintln!("chain at height {h} after {:?}; miner accepted {}", t0.elapsed(), m.status().accepted);
    if mine {
        let _ = m.stop(&emit);
    }
    assert!(h >= want, "the isolated chain did not reach {want} blocks in time (height {h})");
    if mine {
        assert!(net.status().used.is_some_and(|u| u.transport == "http"), "the miner reached the node through the proxy");
    }

    // sync and send
    ok(w("refresh", json!({})));
    let bal = ok(w("get_balance", json!({})));
    let unlocked = bal["unlocked_balance"].as_u64().unwrap();
    assert!(unlocked > 0, "mined coins are unlocked after the 6-block lock: {bal}");
    let sent = ok(w("transfer", json!({"destinations": [{"amount": unlocked / 10, "address": addr2}], "priority": 1, "get_tx_key": true})));
    let txid = sent["tx_hash"].as_str().unwrap().to_string();
    eprintln!("sent {txid}");
    assert_eq!(net.status().used.unwrap().transport, "http");
    // the node took it: it is in the pool (the node's own RPC, not the proxy)
    let pool = ureq::post(&format!("http://127.0.0.1:{}/get_transaction_pool_hashes", a.rpc)).send_json(json!({})).unwrap().into_json::<Value>().unwrap();
    assert!(pool["tx_hashes"].as_array().is_some_and(|l| l.iter().any(|x| x == &json!(txid))), "{pool}");
    // wallet 1 sees its own pending spend (get_transfers pool)
    let t = ok(w("get_transfers", json!({"pool": true, "out": true, "pending": true})));
    assert!(t.to_string().contains(&txid), "{t}");
    // wallet 2 sees the incoming transaction in the pool
    ok(w("close_wallet", json!({})));
    ok(w("open_wallet", json!({"filename": "w2", "password": ""})));
    ok(w("refresh", json!({})));
    let t2 = ok(w("get_transfers", json!({"pool": true, "in": true})));
    assert!(t2.to_string().contains(&txid), "wallet 2 sees the incoming transaction: {t2}");

    // verification of the mined chain (header layer through the proxy's net; the solo chain's rules are the real ones)
    let cp = sm64coin_lc::verify::Checkpoint::genesis(network);
    #[cfg(not(mobile))]
    {
        let rom2 = rom.clone();
        net.enable_replay(Arc::new(move || std::fs::read(&rom2).ok()));
    }
    tauri::async_runtime::block_on(net.verify_now(Some(cp))).expect("verify");
    let v = net.verification();
    eprintln!("verification: {v:?}");
    assert_eq!(v.state, "verified", "{v:?}");
    assert!(v.height.unwrap() + 1 >= want);
    #[cfg(not(mobile))]
    {
        // desktop: the current cycle replays in the background (the ROM is found through $SM64COIN_ROM)
        let t = std::time::Instant::now();
        while net.verification().replay.state == "running" && t.elapsed() < Duration::from_secs(300) {
            std::thread::sleep(Duration::from_secs(2));
        }
        eprintln!("replay: {:?}", net.verification().replay);
    }
}

/// The tip hub against an isolated offline node: followed by push (Server-Sent Events from a tip service in front of it), and, with a
/// node list that has no tip service, by the 30 s poll's first look; the verifier's input and the page's `tip` event both read it.
#[test]
fn the_tip_hub_follows_a_node_by_push_and_by_poll() {
    let Some(a) = start_daemon("tip") else {
        eprintln!("SKIPPED: set SM64COIND and SM64COIN_ROM (or unpack a release bundle in ~/apps)");
        return;
    };
    let network = wait_for(&a);
    let wait_tip = |net: &Arc<Net>| {
        let hub = net.tip_hub();
        for _ in 0..80 {
            if let Some(t) = hub.current() {
                return t;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        panic!("no tip within 20 s");
    };
    // by push, from a tip service
    let sse = tauri::async_runtime::block_on(tip_service(&a));
    let net = net_with(&[(http_of(&a), Source::Bootstrap), (sse, Source::Bootstrap)]);
    tauri::async_runtime::spawn(net.clone().tip_task());
    let t = wait_tip(&net);
    assert_eq!((t.height, t.hash), (0, network), "genesis is the tip of a fresh chain");
    assert_eq!(t.difficulty, 1);
    assert!(t.server_time > 1_700_000_000, "the server stamps its clock (the genesis block itself is timestamped 0)");
    let json = crate::lctip::tip_json(&t);
    assert_eq!(json["hash"], hex::encode(network));
    assert_eq!(json["difficulty"], "1", "big numbers go to the page as strings");
    assert!(net.tip_hub().live_tip_hash().is_some(), "the tip arrived by push");
    // by poll (the node serves no /lc/tip)
    let net2 = net_with(&[(http_of(&a), Source::Bootstrap)]);
    tauri::async_runtime::spawn(net2.clone().tip_task());
    let t2 = wait_tip(&net2);
    assert_eq!((t2.height, t2.hash, t2.timestamp), (t.height, t.hash, t.timestamp), "the same tip by either way");
    // the hub is what the verifier reads: stranded is clear while nodes answer
    assert!(!net.tip_hub().stranded());
}
