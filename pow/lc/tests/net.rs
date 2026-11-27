//! In-process network tests: iroh endpoints with static addressing (no discovery, no n0), a local
//! self-signed relay, a fake node RPC for the fast cases and an isolated offline `sm64coind` for the
//! end-to-end one. Nothing here touches the real network or chain.
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, SecretKey};
use serde_json::{Value, json};
use sm64coin_lc::client::*;
use sm64coin_lc::endpoint::{EndpointOptions, build_endpoint};
use sm64coin_lc::exec::{UpError, Upstream};
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::nodes::{NodeAddr, NodeList, Source};
use sm64coin_lc::proto::*;
use sm64coin_lc::relay::*;
use sm64coin_lc::server::*;

const GENESIS: [u8; 32] = [7; 32];

#[derive(Default)]
struct FakeUp {
    calls: AtomicUsize,
    height: Mutex<u64>,
}

#[async_trait::async_trait]
impl Upstream for FakeUp {
    async fn json_rpc(&self, method: &str, _params: Value, _t: Duration) -> Result<Value, UpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let h = *self.height.lock().unwrap();
        let result = match method {
            "get_info" => json!({"height": h + 1, "top_block_hash": hex::encode([9u8; 32]), "cumulative_difficulty": 42}),
            "get_block" => json!({"blob": "00", "height": 1}),
            "get_fee_estimate" => json!({"fee": 1}),
            _ => json!({}),
        };
        Ok(json!({"result": result}))
    }
    async fn post(&self, _p: &str, _c: &str, body: Vec<u8>, _t: Duration) -> Result<Vec<u8>, UpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(body)
    }
}

struct Srv {
    ep: Endpoint,
    router: iroh::protocol::Router,
    svc: LcService,
    up: Arc<FakeUp>,
}

fn cfg(limits: Limits) -> ServerConfig {
    ServerConfig { network: GENESIS, limits, flags: 0, version: 1, https_rpc: None }
}

async fn serve(upstream: Arc<dyn Upstream>, relay: Option<(&RelayHandle, &str)>, limits: Limits) -> (Endpoint, iroh::protocol::Router, LcService) {
    let secret = SecretKey::generate();
    let mut o = EndpointOptions { secret_key: Some(secret.clone()), n0_fallback: false, alpns: vec![ALPN.to_vec()], ..Default::default() };
    if let Some((h, _)) = relay {
        o.relays = vec![format!("https://127.0.0.1:{}", h.https_addr().unwrap().port()).parse().unwrap()];
        o.only_trust_certs = vec![h.self_signed_cert.clone().unwrap()];
    }
    let ep = build_endpoint(o).await.unwrap();
    let svc = LcService::new(cfg(limits), upstream, secret, HelloLog::default());
    svc.refresh_tip().await;
    let router = svc.serve(ep.clone());
    (ep, router, svc)
}

async fn fake_srv(relay: Option<(&RelayHandle, &str)>, limits: Limits) -> Srv {
    let up = Arc::new(FakeUp::default());
    *up.height.lock().unwrap() = 4;
    let (ep, router, svc) = serve(up.clone(), relay, limits).await;
    Srv { ep, router, svc, up }
}

async fn client_ep(relay: Option<&RelayHandle>) -> Endpoint {
    let mut o = EndpointOptions { n0_fallback: false, ..Default::default() };
    if let Some(h) = relay {
        o.relays = vec![format!("https://127.0.0.1:{}", h.https_addr().unwrap().port()).parse().unwrap()];
        o.only_trust_certs = vec![h.self_signed_cert.clone().unwrap()];
    }
    build_endpoint(o).await.unwrap()
}

fn direct_addr(s: &Endpoint) -> EndpointAddr {
    let mut a = EndpointAddr::new(s.id());
    for b in s.bound_sockets() {
        if b.is_ipv4() {
            a = a.with_ip_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), b.port()));
        }
    }
    a
}

async fn relay_handle(max_clients: usize, policy: HelloPolicy) -> RelayHandle {
    let cfg = RelayConfig { https_bind: ([127, 0, 0, 1], 0).into(), tls: TlsMode::SelfSigned, hello_policy: policy, max_clients, ..Default::default() };
    RelayHandle::spawn(&cfg, HelloLog::default()).await.unwrap()
}

const T: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread")]
async fn direct_hello_tip_and_requests() {
    let s = fake_srv(None, Limits::default()).await;
    let c = client_ep(None).await;
    let t = IrohTransport::new(c.clone(), direct_addr(&s.ep), GENESIS);
    match t.request(&Request::GetTip, T).await.unwrap() {
        Response::Tip(tip) => {
            assert_eq!(tip.height, 4);
            assert_eq!(tip.hash, [9; 32]);
            assert_eq!(tip.cumulative_difficulty, 42);
        }
        r => panic!("{r:?}"),
    }
    assert!(matches!(t.request(&Request::GetFee, T).await.unwrap(), Response::Json(_)));
    match t.request(&Request::GetBlocks { from: 0, count: 3, max_bytes: 1 << 20 }, T).await.unwrap() {
        Response::Blocks(b) => assert_eq!(b.len(), 3),
        r => panic!("{r:?}"),
    }
    match t.request(&Request::Wallet { call: WalletBin::GetOuts, body: vec![1, 2, 3] }, T).await.unwrap() {
        Response::Bin(b) => assert_eq!(b, vec![1, 2, 3]),
        r => panic!("{r:?}"),
    }
    // out-of-range requests are refused before reaching the node
    let before = s.up.calls.load(Ordering::SeqCst);
    assert!(matches!(t.request(&Request::GetHeaders { from: 0, count: 5000 }, T).await.unwrap(), Response::Error(ErrorKind::Malformed)));
    assert_eq!(s.up.calls.load(Ordering::SeqCst), before);
    // GetNodes returns the server's own signed announcement
    match t.request(&Request::GetNodes { max: 8 }, T).await.unwrap() {
        Response::Nodes(v) => {
            assert_eq!(v.len(), 1);
            assert_eq!(v[0].endpoint_id, *s.ep.id().as_bytes());
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
            v[0].verify(now).unwrap();
            let mut forged = v[0].clone();
            forged.height += 1;
            assert!(forged.verify(now).is_err());
            assert!(v[0].verify(now + 3 * ANNOUNCEMENT_TTL_SECS).is_err());
        }
        r => panic!("{r:?}"),
    }
    // Probe over a direct path: observed IP is loopback, and a listening port is reachable
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    match t.request(&Request::Probe { tcp_port: port }, T).await.unwrap() {
        Response::Probe { observed, reachable } => {
            assert_eq!(observed.as_deref(), Some("127.0.0.1"));
            assert!(reachable);
        }
        r => panic!("{r:?}"),
    }
    drop(l);
    c.close().await;
    s.router.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_only_path() {
    let relay = relay_handle(50, HelloPolicy::Off).await;
    let s = fake_srv(Some((&relay, "")), Limits::default()).await;
    s.ep.online().await;
    let c = client_ep(Some(&relay)).await;
    // the address carries the relay URL only: no direct addresses
    let url = format!("https://127.0.0.1:{}", relay.https_addr().unwrap().port());
    let addr = EndpointAddr::new(s.ep.id()).with_relay_url(url.parse().unwrap());
    let t = IrohTransport::new(c.clone(), addr, GENESIS);
    assert!(matches!(t.request(&Request::GetTip, T).await.unwrap(), Response::Tip(_)));
    assert!(relay.clients() >= 1);
    c.close().await;
    s.router.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn failover_between_two_servers() {
    let a = fake_srv(None, Limits::default()).await;
    let b = fake_srv(None, Limits::default()).await;
    let c = client_ep(None).await;
    let mut list = NodeList::new();
    let (na, nb) = (from_endpoint_addr(&direct_addr(&a.ep)), from_endpoint_addr(&direct_addr(&b.ep)));
    list.add(na.clone(), Source::Bootstrap, 0);
    list.add(nb.clone(), Source::Bootstrap, 0);
    let client = Client::new(list, Arc::new(IrohConnector::new(c.clone(), GENESIS)), Arc::new(SystemEnv));
    assert!(client.read(&Request::GetTip, Duration::from_secs(5)).await.is_ok());
    // kill A: reads must still succeed (via B) however the weighted pick falls
    a.router.shutdown().await.unwrap();
    a.ep.close().await;
    for _ in 0..6 {
        let (key, r) = client.read(&Request::GetTip, Duration::from_secs(3)).await.expect("failover");
        assert!(matches!(r, Response::Tip(_)));
        assert_eq!(key, nb.key(), "only B is alive");
    }
    let ra = client.with_list(|l| l.get(&na.key()).cloned().unwrap());
    assert!(ra.strikes > 0 || ra.ok_rate < 0.9, "A was scored down: {ra:?}");
    // submissions fan out to both listed nodes; the dead one fails, the live one answers
    let res = client.submit(&Request::SubmitBlock { blob: vec![1] }, Duration::from_secs(3)).await.unwrap();
    assert_eq!(res.len(), 2);
    assert!(res.iter().any(|(k, r)| *k == nb.key() && r.is_ok()));
    b.router.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_network_is_refused_and_banned() {
    let s = fake_srv(None, Limits::default()).await;
    let c = client_ep(None).await;
    let t = IrohTransport::new(c.clone(), direct_addr(&s.ep), [1; 32]);
    assert!(matches!(t.request(&Request::GetTip, T).await, Err(TransportError::WrongNetwork)));
    let mut list = NodeList::new();
    let n = from_endpoint_addr(&direct_addr(&s.ep));
    list.add(n.clone(), Source::Bootstrap, 0);
    let client = Client::new(list, Arc::new(IrohConnector::new(c, [1; 32])), Arc::new(SystemEnv));
    assert!(client.read(&Request::GetTip, Duration::from_secs(3)).await.is_err());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    assert!(client.with_list(|l| l.candidates(now).is_empty()), "other-chain node banned");
}

/// A raw stream: bytes in, one answer frame out.
async fn raw(conn: &iroh::endpoint::Connection, bytes: &[u8]) -> Result<Response, String> {
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    send.write_all(bytes).await.map_err(|e| e.to_string())?;
    send.finish().map_err(|e| e.to_string())?;
    let body = read_frame(&mut recv, MAX_FRAME).await.map_err(|e| e.to_string())?.ok_or("no answer")?;
    decode_body(&body).map_err(|e| e.to_string())
}

fn frame(r: &Request) -> Vec<u8> {
    encode_frame(r, MAX_REQUEST_FRAME).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_oversized_and_unhelloed() {
    let s = fake_srv(None, Limits::default()).await;
    let c = client_ep(None).await;
    let conn = c.connect(direct_addr(&s.ep), ALPN).await.unwrap();
    // before Hello
    assert_eq!(raw(&conn, &frame(&Request::GetTip)).await.unwrap(), Response::Error(ErrorKind::BadHello));
    assert!(matches!(raw(&conn, &frame(&Request::Hello { minor: 0, features: u64::MAX, network: GENESIS })).await.unwrap(), Response::Hello { .. }), "unknown feature bits are ignored");
    assert!(matches!(raw(&conn, &frame(&Request::GetTip)).await.unwrap(), Response::Tip(_)));
    // garbage that is a valid frame but not a request
    let mut bad = vec![0, 0, 0, 3, 0xff, 0xff, 0xff];
    assert_eq!(raw(&conn, &bad).await.unwrap(), Response::Error(ErrorKind::Malformed));
    // a frame whose length prefix lies (claims more than is sent): stream ends, no answer, server survives
    bad = vec![0, 0, 1, 0, 1, 2];
    assert!(raw(&conn, &bad).await.is_err());
    assert!(matches!(raw(&conn, &frame(&Request::GetTip)).await.unwrap(), Response::Tip(_)));
    // oversized length prefix: TooLarge, and the connection is closed (nothing was allocated for 4 GiB)
    let huge = [0xffu8, 0xff, 0xff, 0xf0, 1, 2, 3];
    assert_eq!(raw(&conn, &huge).await.unwrap(), Response::Error(ErrorKind::TooLarge));
    tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.expect("server closes after an oversized frame");
    // repeated malformed frames close the connection too
    let conn = c.connect(direct_addr(&s.ep), ALPN).await.unwrap();
    let _ = raw(&conn, &frame(&Request::Hello { minor: 0, features: 0, network: GENESIS })).await.unwrap();
    for _ in 0..3 {
        let _ = raw(&conn, &[0, 0, 0, 2, 0xff, 0xfe]).await;
    }
    tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.expect("closed after repeated malformed");
    assert!(s.svc.stats().malformed.load(Ordering::Relaxed) >= 4);
    // wrong network Hello is answered and closed
    let conn = c.connect(direct_addr(&s.ep), ALPN).await.unwrap();
    assert_eq!(raw(&conn, &frame(&Request::Hello { minor: 0, features: 0, network: [0; 32] })).await.unwrap(), Response::Error(ErrorKind::BadHello));
    tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.expect("closed after wrong network");
    // a request frame above the 1 MiB request cap is refused client-side by the codec
    assert!(encode_frame(&Request::SubmitBlock { blob: vec![0; MAX_REQUEST_FRAME + 1] }, MAX_REQUEST_FRAME).is_err());
    s.router.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limit_answers_busy_before_touching_the_node() {
    let limits = Limits { burst: 25.0, refill_per_sec: 0.5, ..Limits::default() };
    let s = fake_srv(None, limits).await;
    let c = client_ep(None).await;
    let conn = c.connect(direct_addr(&s.ep), ALPN).await.unwrap();
    raw(&conn, &frame(&Request::Hello { minor: 0, features: 0, network: GENESIS })).await.unwrap();
    let before = s.up.calls.load(Ordering::SeqCst);
    let (mut ok, mut busy) = (0, 0);
    for _ in 0..10 {
        // GetBlocks costs 10 tokens: the burst of 25 allows two
        match raw(&conn, &frame(&Request::GetBlocks { from: 0, count: 1, max_bytes: 1000 })).await.unwrap() {
            Response::Blocks(_) => ok += 1,
            Response::Error(ErrorKind::Busy { retry_ms }) => {
                assert!(retry_ms > 0);
                busy += 1;
            }
            r => panic!("{r:?}"),
        }
    }
    assert_eq!((ok, busy), (2, 8), "token bucket: burst then Busy");
    assert_eq!(s.up.calls.load(Ordering::SeqCst) - before, 2, "Busy requests never reached the node");
    // another endpoint has its own bucket
    let c2 = client_ep(None).await;
    let conn2 = c2.connect(direct_addr(&s.ep), ALPN).await.unwrap();
    raw(&conn2, &frame(&Request::Hello { minor: 0, features: 0, network: GENESIS })).await.unwrap();
    // (same IP, so the shared IP bucket (2x burst) applies, but one request still fits)
    assert!(matches!(raw(&conn2, &frame(&Request::GetTip)).await.unwrap(), Response::Tip(_)));
    s.router.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_max_clients_and_hello_policy() {
    // strict: strangers are refused at the relay
    let relay = relay_handle(50, HelloPolicy::Strict).await;
    let secret = SecretKey::generate();
    let ep = build_endpoint(EndpointOptions {
        secret_key: Some(secret),
        relays: vec![format!("https://127.0.0.1:{}", relay.https_addr().unwrap().port()).parse().unwrap()],
        n0_fallback: false,
        only_trust_certs: vec![relay.self_signed_cert.clone().unwrap()],
        ..Default::default()
    })
    .await
    .unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(4), ep.online()).await.is_err(), "stranger never gets online under Strict");
    assert_eq!(relay.clients(), 0, "stranger denied under Strict");
    ep.close().await;
    // max_clients = 1 under Off: the second endpoint is refused
    let relay = relay_handle(1, HelloPolicy::Off).await;
    let mk = || async {
        build_endpoint(EndpointOptions {
            relays: vec![format!("https://127.0.0.1:{}", relay.https_addr().unwrap().port()).parse().unwrap()],
            n0_fallback: false,
            only_trust_certs: vec![relay.self_signed_cert.clone().unwrap()],
            ..Default::default()
        })
        .await
        .unwrap()
    };
    let e1 = mk().await;
    tokio::time::timeout(Duration::from_secs(10), e1.online()).await.expect("first client admitted");
    let e2 = mk().await;
    assert!(tokio::time::timeout(Duration::from_secs(4), e2.online()).await.is_err(), "second client refused: relay full");
    assert_eq!(relay.clients(), 1);
    e1.close().await;
    e2.close().await;
}

struct FixedPeers(Vec<(String, Option<IpAddr>, Arc<dyn Transport>)>);
#[async_trait::async_trait]
impl PeerSource for FixedPeers {
    async fn peers(&self) -> Vec<(String, Option<IpAddr>, Arc<dyn Transport>)> {
        self.0.clone()
    }
}

struct Canned(Response);
#[async_trait::async_trait]
impl Transport for Canned {
    async fn request(&self, _r: &Request, _t: Duration) -> Result<Response, TransportError> {
        Ok(self.0.clone())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_controller_auto_enable_and_disable() {
    let ip = |s: &str| -> Option<IpAddr> { Some(s.parse().unwrap()) };
    let yes = |pubip: &str| -> Arc<dyn Transport> { Arc::new(Canned(Response::Probe { observed: Some(pubip.into()), reachable: true })) };
    let no: Arc<dyn Transport> = Arc::new(Canned(Response::Probe { observed: Some("203.0.113.9".into()), reachable: false }));
    let s = fake_srv(None, Limits::default()).await;
    let rcfg = RelayConfig { https_bind: ([127, 0, 0, 1], 0).into(), ..Default::default() };

    // two reachable answers from different /16s: relay enabled and advertised
    let peers = Arc::new(FixedPeers(vec![("a".into(), ip("198.51.100.1"), yes("203.0.113.9")), ("b".into(), ip("192.0.2.1"), yes("203.0.113.9"))]));
    let mut ctl = RelayController::new(RelayChoice::Auto, rcfg.clone(), None, HelloLog::default(), s.svc.clone(), s.ep.clone(), peers);
    assert!(matches!(ctl.step().await, RelayState::Active { .. }), "{:?}", ctl.state());
    assert!(ctl.handle().is_some());
    assert!(s.svc.announcement().unwrap().flags & node_flags::RELAY != 0);

    // one failure of two: stays off
    let peers = Arc::new(FixedPeers(vec![("a".into(), ip("198.51.100.1"), yes("203.0.113.9")), ("b".into(), ip("192.0.2.1"), no.clone())]));
    let mut ctl = RelayController::new(RelayChoice::Auto, rcfg.clone(), None, HelloLog::default(), s.svc.clone(), s.ep.clone(), peers);
    assert!(matches!(ctl.step().await, RelayState::Failed(_)));
    assert!(ctl.handle().is_none());
    assert!(s.svc.announcement().unwrap().flags & node_flags::RELAY == 0);

    // explicit Off never relays, explicit On always does
    let mut off = RelayController::new(RelayChoice::Off, rcfg.clone(), None, HelloLog::default(), s.svc.clone(), s.ep.clone(), Arc::new(FixedPeers(vec![])));
    assert_eq!(*off.step().await, RelayState::Off);
    let mut on = RelayController::new(RelayChoice::On, rcfg, Some("localhost".into()), HelloLog::default(), s.svc.clone(), s.ep.clone(), Arc::new(FixedPeers(vec![])));
    assert!(matches!(on.step().await, RelayState::Active { .. }));
    s.router.shutdown().await.unwrap();
}

#[test]
fn reachability_decision() {
    let o = |id: &str, peer: &str, obs: &str, ok: bool| ProbeOutcome { peer_ip: Some(peer.parse().unwrap()), peer_id: id.into(), observed: Some(obs.parse().unwrap()), reachable: ok };
    assert!(matches!(decide(&[o("a", "1.1.1.1", "8.8.8.8", true), o("b", "2.2.2.2", "8.8.8.8", true)]), Verdict::Reachable { .. }));
    assert!(matches!(decide(&[o("a", "1.1.1.1", "8.8.8.8", true), o("b", "1.1.9.9", "8.8.8.8", true)]), Verdict::Unreachable(_)), "same /16");
    assert!(matches!(decide(&[o("a", "1.1.1.1", "8.8.8.8", true)]), Verdict::Unreachable(_)));
    assert!(matches!(decide(&[o("a", "1.1.1.1", "192.168.0.5", true), o("b", "2.2.2.2", "192.168.0.5", true)]), Verdict::Unreachable(_)), "private IP");
    assert!(matches!(decide(&[o("a", "1.1.1.1", "8.8.8.8", true), o("b", "2.2.2.2", "9.9.9.9", true)]), Verdict::Unreachable(_)), "disagreement");
    assert!(is_non_public("100.64.1.1".parse().unwrap()), "CGNAT");
}

#[test]
fn monthly_usage_and_acme_config() {
    assert_eq!(month_id(0), 197001);
    assert_eq!(month_id(1_795_000_000), 202611);
    let mut u = Usage::default();
    assert_eq!(u.update(1000, 1_795_000_000), 1000);
    assert_eq!(u.update(1500, 1_795_000_100), 1500);
    assert_eq!(u.update(1600, 1_795_000_000 + 40 * 86400), 100, "a new month starts again");
    // ACME config builds (no network is touched until a server uses it)
    let acme = TlsMode::Acme { domains: vec!["relay.example".into()], contact: vec!["mailto:a@example".into()], cache_dir: std::env::temp_dir().join("lc-acme-test"), production: false };
    assert!(matches!(cert_config(&acme).unwrap().0, iroh_relay::server::CertConfig::LetsEncrypt { .. }));
    assert!(cert_config(&TlsMode::Acme { domains: vec![], contact: vec![], cache_dir: "x".into(), production: false }).is_err());
}

#[tokio::test]
async fn http_upstream_refuses_non_allowlisted() {
    let up = HttpUpstream::new("http://127.0.0.1:1").unwrap();
    assert!(matches!(up.json_rpc("stop_daemon", json!({}), Duration::from_secs(1)).await, Err(UpError::NotAllowed(_))));
    assert!(matches!(up.post("/stop_daemon", "application/json", vec![], Duration::from_secs(1)).await, Err(UpError::NotAllowed(_))));
    assert!(HttpUpstream::new("ftp://x").is_err());
}

// ------------------------------------------------------------ the real (isolated, offline) node

struct Daemon {
    child: Child,
    rpc: u16,
    _dir: std::path::PathBuf,
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn start_daemon() -> Option<Daemon> {
    let home = std::env::var("HOME").ok()?;
    let dir = std::path::PathBuf::from(std::env::var("SM64COIN_APP_DIR").unwrap_or(format!("{home}/apps/sm64coin-0.5.0-linux-x86_64")));
    let (bin, rom) = (dir.join("sm64coind"), dir.join("baserom.us.z64"));
    if !bin.exists() || !rom.exists() {
        eprintln!("sm64coind not found at {dir:?}: skipping the real-node test");
        return None;
    }
    let data = std::env::temp_dir().join(format!("lc-test-node-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).unwrap();
    let (rpc, p2p, zmq) = (free_port(), free_port(), free_port());
    let child = Command::new(bin)
        .args(["--offline", "--sm64-solo", "--no-igd", "--non-interactive", "--p2p-bind-ip", "127.0.0.1", "--rpc-bind-ip", "127.0.0.1"])
        .args(["--rpc-bind-port", &rpc.to_string(), "--p2p-bind-port", &p2p.to_string(), "--zmq-rpc-bind-port", &zmq.to_string()])
        .arg("--data-dir").arg(&data).arg("--sm64-rom").arg(rom).arg("--log-file").arg(data.join("d.log"))
        .stdout(Stdio::null()).stderr(Stdio::null())
        .spawn()
        .ok()?;
    Some(Daemon { child, rpc, _dir: data })
}

#[tokio::test(flavor = "multi_thread")]
async fn end_to_end_against_an_isolated_offline_node() {
    let Some(d) = start_daemon() else { return };
    let up = Arc::new(HttpUpstream::new(&format!("http://127.0.0.1:{}", d.rpc)).unwrap());
    let mut genesis = Err(String::new());
    for _ in 0..40 {
        genesis = sm64coin_lc::exec::genesis_hash(&*up, Duration::from_secs(2)).await;
        if genesis.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let network = genesis.expect("the offline node answers");
    let secret = SecretKey::generate();
    let ep = build_endpoint(EndpointOptions { secret_key: Some(secret.clone()), n0_fallback: false, alpns: vec![ALPN.to_vec()], ..Default::default() }).await.unwrap();
    let svc = LcService::new(ServerConfig { network, limits: Limits::default(), flags: 0, version: 1, https_rpc: None }, up.clone(), secret, HelloLog::default());
    svc.refresh_tip().await;
    let router = svc.serve(ep.clone());
    let c = client_ep(None).await;
    let t = IrohTransport::new(c.clone(), direct_addr(&ep), network);

    let tip = match t.request(&Request::GetTip, T).await.unwrap() {
        Response::Tip(t) => t,
        r => panic!("{r:?}"),
    };
    assert_eq!(tip.height, 0, "a fresh chain holds only genesis");
    assert_eq!(tip.hash, network);
    assert!(matches!(t.request(&Request::GetHeaders { from: 0, count: 1 }, T).await.unwrap(), Response::Json(j) if j.contains("headers")));
    match t.request(&Request::GetBlocks { from: 0, count: 1, max_bytes: 1 << 20 }, T).await.unwrap() {
        Response::Blocks(b) => assert!(b[0].contains("blob")),
        r => panic!("{r:?}"),
    }
    assert!(matches!(t.request(&Request::GetCycle { hash: String::new() }, T).await.unwrap(), Response::Json(j) if j.contains("played_pads")));
    assert!(matches!(t.request(&Request::GetFee, T).await.unwrap(), Response::Json(_)));
    let addr = "46Bwtj7Xrd1KW4GoPEbs2YLB8z6fSKjdZMXPYG5E3nsxHsCKYDSVQxZWf1k8Kx5SXZDjNhfRXj1CjpKPYBYRQDjA8tPDSUH";
    let tpl = t.request(&Request::GetTemplate { address: addr.into(), reserve: 8 }, T).await.unwrap();
    assert!(matches!(tpl, Response::Json(_) | Response::Error(ErrorKind::Upstream(_))), "a template or the node's own address error: {tpl:?}");
    assert!(matches!(t.request(&Request::SubmitBlock { blob: vec![0, 1, 2] }, T).await.unwrap(), Response::Rejected { .. } | Response::Error(ErrorKind::Upstream(_))));
    // the HTTP transport over the same node gives the same tip
    let http = UpstreamTransport(HttpUpstream::new(&format!("http://127.0.0.1:{}", d.rpc)).unwrap());
    assert_eq!(http.request(&Request::GetTip, T).await.unwrap(), Response::Tip(tip));
    c.close().await;
    router.shutdown().await.unwrap();
}

#[allow(dead_code)]
fn _unused(_: NodeAddr) {}
