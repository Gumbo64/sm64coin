//! Failover of the app's light-client network, against ISOLATED nodes only: two `sm64coind --offline --sm64-solo` on fresh data
//! dirs and loopback ports, each behind an lc service (`LcService`) on a local self-signed relay, static addressing (no
//! discovery, no n0), plus dead nodes (a closed port, an endpoint that is not there). Nothing here touches the real network or
//! chain. Needs the daemon and the ROM (the unpacked release bundle in ~/apps/sm64coin-*, or SM64COIND and SM64COIN_ROM);
//! skips, and says so, without them.
//!
//!   cargo test --lib lcnet_test -- --nocapture
use crate::lcnet::{MAINNET_GENESIS, Net};
use crate::remote::Remote;
use crate::walletproxy;
use iroh::{EndpointAddr, SecretKey};
use serde_json::{Value, json};
use sm64coin_lc::endpoint::{Discovery, EndpointOptions, build_endpoint};
use sm64coin_lc::exec::{self, Upstream};
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::nodes::{NodeAddr, Source};
use sm64coin_lc::proto::{ALPN, node_flags};
use sm64coin_lc::relay::{HelloPolicy, RelayConfig, RelayHandle, TlsMode};
use sm64coin_lc::server::{HelloLog, LcService, Limits, ServerConfig};
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

fn start_daemon(tag: &str) -> Option<Daemon> {
    let bin = std::env::var_os("SM64COIND").map(PathBuf::from).or_else(|| bundle_file("sm64coind"))?;
    let rom = std::env::var_os("SM64COIN_ROM").map(PathBuf::from).or_else(|| bundle_file("baserom.us.z64"))?;
    let dir = std::env::temp_dir().join(format!("lcnet-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
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

/// An lc service in front of `d`, on the local relay; returns what a client needs to reach it.
struct Server {
    addr: NodeAddr,
    _router: iroh::protocol::Router,
}

fn http_of(d: &Daemon) -> NodeAddr {
    NodeAddr::Http { url: format!("http://127.0.0.1:{}", d.rpc) }
}

fn relay_opts(relay: &RelayHandle) -> EndpointOptions {
    EndpointOptions {
        relays: vec![format!("https://127.0.0.1:{}", relay.https_addr().unwrap().port()).parse().unwrap()],
        only_trust_certs: vec![relay.self_signed_cert.clone().unwrap()],
        n0_fallback: false,
        discovery: Discovery::None,
        ..Default::default()
    }
}

async fn serve(d: &Daemon, relay: &RelayHandle, network: [u8; 32]) -> Server {
    let secret = SecretKey::generate();
    let mut o = relay_opts(relay);
    o.secret_key = Some(secret.clone());
    o.alpns = vec![ALPN.to_vec()];
    let relay_url = o.relays[0].to_string();
    let ep = build_endpoint(o).await.unwrap();
    let up: Arc<dyn Upstream> = Arc::new(HttpUpstream::new(&format!("http://127.0.0.1:{}", d.rpc)).unwrap());
    let svc = LcService::new(ServerConfig { network, limits: Limits::default(), flags: node_flags::ARCHIVE, version: 1, https_rpc: None }, up, secret, HelloLog::default());
    svc.refresh_tip().await;
    let router = svc.serve(ep.clone());
    // static addressing: the id, the home relay and the loopback socket
    let mut a = EndpointAddr::new(ep.id()).with_relay_url(relay_url.parse().unwrap());
    for b in ep.bound_sockets().into_iter().filter(|b| b.is_ipv4()) {
        a = a.with_ip_addr(std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), b.port()));
    }
    Server { addr: sm64coin_lc::client::from_endpoint_addr(&a), _router: router }
}

fn dead_http() -> NodeAddr {
    NodeAddr::Http { url: format!("http://127.0.0.1:{}", free_port()) } // nothing listens
}

fn dead_iroh(relay: &RelayHandle) -> NodeAddr {
    // a valid endpoint id nobody runs, "behind" the local relay
    let id = SecretKey::generate().public();
    NodeAddr::Iroh { endpoint_id: id.to_string(), relay_url: Some(format!("https://127.0.0.1:{}", relay.https_addr().unwrap().port())), direct: vec![] }
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
fn http_and_iroh_failover_through_remote_and_the_wallet_proxy() {
    let (Some(a), Some(b)) = (start_daemon("a"), start_daemon("b")) else {
        eprintln!("SKIPPED: set SM64COIND and SM64COIN_ROM (or unpack a release bundle in ~/apps)");
        return;
    };
    // wait for both isolated nodes and take the genesis hash (the `network` of the iroh Hello)
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

    let relay = tauri::async_runtime::block_on(RelayHandle::spawn(&RelayConfig { https_bind: ([127, 0, 0, 1], 0).into(), tls: TlsMode::SelfSigned, hello_policy: HelloPolicy::Off, ..Default::default() }, HelloLog::default())).unwrap();
    let (sa, sb) = tauri::async_runtime::block_on(async { (serve(&a, &relay, network).await, serve(&b, &relay, network).await) });

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
        assert!(dead.ok_rate < 0.5, "the dead node was scored down: {dead:?}");

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

    // ---------------------------------------------------------------- iroh: a dead endpoint, then a live one (and the relay path)
    {
        let net = net_with(&[(dead_iroh(&relay), Source::Bootstrap), (sa.addr.clone(), Source::Bootstrap)]);
        // before the endpoint is up an iroh node has no transport: the call fails cleanly, and nothing is banned for it forever
        assert!(tauri::async_runtime::block_on(net.rpc("get_fee_estimate", json!({}))).is_err());
        let id = tauri::async_runtime::block_on(net.start_iroh(relay_opts(&relay), network)).unwrap();
        assert!(net.status().iroh.as_deref() == Some(id.as_str()));
        let proxy = walletproxy::start(net.clone(), vec![]).unwrap();
        let url = proxy.url();
        let mut ok = 0;
        for _ in 0..8 {
            // get_fee_estimate and the wallet's binary calls have protocol messages: they fail over between iroh nodes
            let v = jrpc(&url, "get_fee_estimate", json!({}));
            assert!(v["result"].get("fee").is_some(), "{v}");
            let (code, body) = post(&url, "/gethashes.bin", "application/octet-stream", &EMPTY_EPEE);
            assert_eq!(code, 200, "{}", String::from_utf8_lossy(&body));
            assert_eq!(&body[..4], &[0x01, 0x11, 0x01, 0x01]);
            ok += 1;
        }
        assert_eq!(ok, 8);
        let used = net.status().used.unwrap();
        assert_eq!(used.transport, "iroh", "{used:?}");
        // the headers of the genesis block through iroh
        let v = jrpc(&url, "get_block_headers_range", json!({"start_height": 0, "end_height": 0}));
        assert!(v["result"]["headers"].is_array(), "{v}");
        // a call with no protocol message needs an HTTP node: with none in the list it says so
        let (code, body) = post(&url, "/json_rpc", "application/json", br#"{"jsonrpc":"2.0","id":1,"method":"get_info","params":{}}"#);
        assert_eq!(code, 502, "{}", String::from_utf8_lossy(&body));
        assert!(String::from_utf8_lossy(&body).contains("no HTTP node"));
        // submissions fan out and report a node's verdict: junk is rejected by the node, not lost
        let v = jrpc(&url, "submit_block", json!(["000102"]));
        assert!(v.get("error").is_some(), "junk block is rejected: {v}");
        drop(proxy);
    }

    // ---------------------------------------------------------------- everything at once, both transports
    {
        let net = net_with(&[(dead_http(), Source::Bootstrap), (dead_iroh(&relay), Source::Bootstrap), (http_of(&b), Source::Bootstrap), (sa.addr.clone(), Source::Bootstrap), (sb.addr.clone(), Source::Bootstrap)]);
        tauri::async_runtime::block_on(net.start_iroh(relay_opts(&relay), network)).unwrap();
        let proxy = walletproxy::start(net.clone(), vec![]).unwrap();
        let url = proxy.url();
        let mut transports = std::collections::BTreeSet::new();
        for _ in 0..24 {
            let v = jrpc(&url, "get_info", json!({})); // HTTP only: the live http node
            assert_eq!(v["result"]["height"], 1);
            let v = jrpc(&url, "get_fee_estimate", json!({})); // any live node
            assert!(v["result"].get("fee").is_some());
            transports.insert(net.status().used.unwrap().transport);
        }
        eprintln!("transports used for fee estimates: {transports:?}");
        let st = net.status();
        assert!(st.nodes.iter().filter(|n| n.ok_rate > 0.5).count() >= 3, "the live nodes are scored up: {:?}", st.nodes);
        assert!(st.nodes.iter().any(|n| n.banned) || st.nodes.iter().any(|n| n.ok_rate < 0.4), "the dead ones are scored down: {:?}", st.nodes);
        drop(proxy);
    }
    tauri::async_runtime::block_on(relay.shutdown());
}
