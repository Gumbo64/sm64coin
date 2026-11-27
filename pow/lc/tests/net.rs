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
    /// The tip hash byte (tests change it to "mine a block").
    hash: Mutex<u8>,
}

#[async_trait::async_trait]
impl Upstream for FakeUp {
    async fn json_rpc(&self, method: &str, _params: Value, _t: Duration) -> Result<Value, UpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let h = *self.height.lock().unwrap();
        let hb = *self.hash.lock().unwrap();
        let result = match method {
            "get_info" => json!({"height": h + 1, "top_block_hash": hex::encode([hb; 32]), "cumulative_difficulty": 42, "difficulty": 1000}),
            "get_last_block_header" => json!({"block_header": {"hash": hex::encode([hb; 32]), "height": h, "timestamp": 1_700_000_000u64 + h}}),
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
    *up.hash.lock().unwrap() = 9;
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
    // submissions fan out to every usable listed node (A is out once three failed reads banned it for an hour, which depends
    // on how the weighted picks fell above); the live one answers
    let now = SystemEnv.now_ms() / 1000;
    let a_banned = client.with_list(|l| l.get(&na.key()).unwrap().is_banned(now));
    let res = client.submit(&Request::SubmitBlock { blob: vec![1] }, Duration::from_secs(3)).await.unwrap();
    assert_eq!(res.len(), if a_banned { 1 } else { 2 });
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

#[test]
fn request_variant_count_matches_the_enum() {
    // the newest variant sits at REQUEST_VARIANTS - 1 (append-only enum; bump the constant with each new variant)
    let last = postcard::to_stdvec(&Request::SubscribeTip).unwrap();
    assert_eq!(last[0] as u32, REQUEST_VARIANTS - 1);
    assert!(!is_unknown_request(&last));
    assert!(is_unknown_request(&[REQUEST_VARIANTS as u8, 1, 2]));
    assert!(is_unknown_request(&[0x80 | 3, 1])); // varint 131
    assert!(!is_unknown_request(&[0xff, 0xff]), "unterminated varint is just malformed");
}

#[tokio::test(flavor = "multi_thread")]
async fn passthrough_json_rpc_and_paths_with_the_allowlist() {
    let s = fake_srv(None, Limits::default()).await;
    let c = client_ep(None).await;
    let t = IrohTransport::new(c.clone(), direct_addr(&s.ep), GENESIS);
    // any allowlisted JSON-RPC method: the node's whole answer comes back
    let params = json!({"wallet_address": "A", "reserve_size": 8, "sm64_frames": 3}).to_string();
    match t.request(&Request::JsonRpc { method: "get_block_template".into(), params }, T).await.unwrap() {
        Response::JsonRpc(j) => assert!(serde_json::from_str::<Value>(&j).unwrap().get("result").is_some()),
        r => panic!("{r:?}"),
    }
    assert!(matches!(t.request(&Request::JsonRpc { method: "get_sm64_state".into(), params: "{}".into() }, T).await.unwrap(), Response::JsonRpc(_)));
    // a path call is forwarded with its body
    assert_eq!(t.request(&Request::Path { path: "/get_transactions".into(), body: b"{\"txs_hashes\":[]}".to_vec() }, T).await.unwrap(), Response::Bin(b"{\"txs_hashes\":[]}".to_vec()));
    assert!(matches!(t.request(&Request::Path { path: "/get_o_indexes.bin".into(), body: vec![1, 2] }, T).await.unwrap(), Response::Bin(_)));
    let calls = s.up.calls.load(Ordering::SeqCst);
    // not on the allowlist: refused without touching the node
    for m in ["stop_daemon", "set_bans", "prune_blockchain", "get_connections"] {
        assert_eq!(t.request(&Request::JsonRpc { method: m.into(), params: "{}".into() }, T).await.unwrap(), Response::Error(ErrorKind::Unsupported), "{m}");
    }
    for p in ["/stop_daemon", "/get_transaction_pool", "/get_peer_list", "/start_mining", "/../json_rpc"] {
        assert_eq!(t.request(&Request::Path { path: p.into(), body: vec![] }, T).await.unwrap(), Response::Error(ErrorKind::Unsupported), "{p}");
    }
    assert_eq!(t.request(&Request::JsonRpc { method: "get_info".into(), params: "not json".into() }, T).await.unwrap(), Response::Error(ErrorKind::Malformed));
    assert_eq!(s.up.calls.load(Ordering::SeqCst), calls);
    s.router.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_request_is_answered_not_a_disconnect() {
    let s = fake_srv(None, Limits::default()).await;
    let c = client_ep(None).await;
    let conn = c.connect(direct_addr(&s.ep), ALPN).await.unwrap();
    assert!(matches!(raw(&conn, &frame(&Request::Hello { minor: 9, features: u64::MAX, network: GENESIS })).await.unwrap(), Response::Hello { .. }));
    // a variant from a future minor (index 40) with some payload; many times over: still answered, connection stays up
    for _ in 0..8 {
        let mut body = vec![40u8, 1, 2, 3];
        let mut f = (body.len() as u32).to_be_bytes().to_vec();
        f.append(&mut body);
        assert_eq!(raw(&conn, &f).await.unwrap(), Response::Error(ErrorKind::Unsupported));
    }
    assert!(conn.close_reason().is_none());
    assert!(matches!(raw(&conn, &frame(&Request::GetTip)).await.unwrap(), Response::Tip(_)));
    assert_eq!(s.svc.stats().malformed.load(Ordering::Relaxed), 0);
    s.router.shutdown().await.unwrap();
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
    let rcfg = RelayConfig { https_bind: ([127, 0, 0, 1], 0).into(), advertise_self_signed: true, ..Default::default() };
    // a self-signed relay is never advertised by the self-test (nobody would trust it) unless allowed
    let peers = Arc::new(FixedPeers(vec![("a".into(), ip("198.51.100.1"), yes("203.0.113.9")), ("b".into(), ip("192.0.2.1"), yes("203.0.113.9"))]));
    let mut ctl = RelayController::new(RelayChoice::Auto, RelayConfig { advertise_self_signed: false, ..rcfg.clone() }, None, HelloLog::default(), s.svc.clone(), s.ep.clone(), peers);
    assert!(matches!(ctl.step().await, RelayState::Failed(_)));

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
    // peers whose address is unknown (relayed) or private do not count as networks
    let u = |id: &str, obs: &str| ProbeOutcome { peer_ip: None, peer_id: id.into(), observed: Some(obs.parse().unwrap()), reachable: true };
    assert!(matches!(decide(&[u("a", "8.8.8.8"), u("b", "8.8.8.8")]), Verdict::Unreachable(_)), "two relayed sybils");
    assert!(matches!(decide(&[o("a", "1.1.1.1", "8.8.8.8", true), u("b", "8.8.8.8")]), Verdict::Unreachable(_)));
    assert!(matches!(decide(&[o("a", "10.0.0.1", "8.8.8.8", true), o("b", "1.1.1.1", "8.8.8.8", true)]), Verdict::Unreachable(_)), "private peer");
    assert!(matches!(decide(&[o("a", "1.1.1.1", "100.100.1.1", true), o("b", "2.2.2.2", "100.100.1.1", true)]), Verdict::Unreachable(_)), "CGNAT observed");
    assert!(matches!(decide(&[o("a", "2001:db8:1::1", "8.8.8.8", true), o("b", "2001:db9::1", "8.8.8.8", true)]), Verdict::Reachable { .. }), "IPv6 /32s");
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

/// A real node behind the service: the pushed tip is the node's (block, timestamp, difficulty), and the limit a client computes
/// from it with the Rust rules is the number the node itself reports for a block found now.
#[cfg(feature = "consensus")]
#[tokio::test(flavor = "multi_thread")]
async fn tip_push_from_a_real_node_gives_the_nodes_own_limit() {
    let Some(d) = start_daemon() else { return };
    let url = format!("http://127.0.0.1:{}", d.rpc);
    let up = Arc::new(HttpUpstream::new(&url).unwrap());
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
    let svc = LcService::new(ServerConfig { network, limits: fast_limits(), flags: 0, version: 1, https_rpc: None }, up.clone(), secret, HelloLog::default());
    svc.refresh_tip().await;
    let router = svc.serve(ep.clone());
    let c = client_ep(None).await;
    let t = IrohTransport::new(c.clone(), direct_addr(&ep), network);
    let mut st = t.open_tip_stream().await.expect("a real node's service pushes");
    let tip = next_tip(&mut st).await;
    assert_eq!((tip.height, tip.hash), (0, network));
    let info = up.json_rpc("get_info", json!({}), T).await.unwrap()["result"].clone();
    let hdr = up.json_rpc("get_last_block_header", json!({}), T).await.unwrap()["result"]["block_header"].clone();
    assert_eq!(tip.timestamp, hdr["timestamp"].as_u64().unwrap());
    assert_eq!(tip.difficulty, info["difficulty"].as_u64().unwrap());
    // the limit from the pushed tip and the clock = the node's get_info number (retry across a second boundary)
    let mut same = false;
    for _ in 0..5 {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let mine = sm64pow::rules().max_frames(tip.difficulty, now.saturating_sub(tip.timestamp));
        let theirs = up.json_rpc("get_info", json!({}), T).await.unwrap()["result"]["sm64_max_frames"].as_u64().unwrap() as u32;
        if mine == theirs {
            same = true;
            break;
        }
    }
    assert!(same, "the local limit equals the node's");
    drop(st);
    c.close().await;
    router.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn gossip_floods_and_garbage_announcements_are_contained() {
    use sm64coin_lc::endpoint::sign_announcement;
    let s = fake_srv(None, Limits::default()).await;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let ann = |relay: &str| {
        let k = SecretKey::generate();
        let addr = EndpointAddr::new(k.public()).with_relay_url(relay.parse().unwrap());
        sign_announcement(&k, &addr, None, 1, 0, 1, now)
    };
    // the server's known set fills up and then keeps what it has (fresh ids are free; they must not push servers out)
    let first = ann("https://relay.example.org");
    assert!(s.svc.add_known(first.clone()));
    for _ in 0..80 {
        s.svc.add_known(ann("https://relay.example.org"));
    }
    let known = match s.svc.announcement() {
        Some(_) => {
            let c = client_ep(None).await;
            let t = IrohTransport::new(c.clone(), direct_addr(&s.ep), GENESIS);
            let r = t.request(&Request::GetNodes { max: MAX_NODES }, T).await.unwrap();
            c.close().await;
            match r {
                Response::Nodes(v) => v,
                other => panic!("{other:?}"),
            }
        }
        None => vec![],
    };
    assert!(known.len() <= MAX_NODES as usize);
    assert!(known.iter().any(|a| a.endpoint_id == first.endpoint_id), "the first known server is kept");
    // signed garbage is refused: a non-https relay, too many addresses
    let mut bad = ann("https://relay.example.org");
    bad.relay_url = Some("http://evil.example".into());
    assert!(bad.validate().is_err());
    let mut many = ann("https://relay.example.org");
    many.direct_addrs = (0..20).map(|i| format!("192.0.2.{i}:1000")).collect();
    assert!(many.validate().is_err());
    assert!(!s.svc.add_known(many));
    // a client takes at most a few new nodes from one answer
    let client = Client::new(NodeList::new(), Arc::new(IrohConnector::new(client_ep(None).await, GENESIS)), Arc::new(SystemEnv));
    let flood: Vec<_> = (0..50).map(|_| ann("https://relay.example.org")).collect();
    assert_eq!(client.learn(&flood), LEARN_PER_ANSWER);
    s.router.shutdown().await.unwrap();
}

// ---- tip push (minor 2) ----

fn fast_limits() -> Limits {
    Limits { tip_poll: Duration::from_millis(100), tip_idle_poll: Duration::from_millis(100), keepalive: Duration::from_millis(300), ..Limits::default() }
}

fn mine(up: &FakeUp, hash: u8) {
    *up.height.lock().unwrap() += 1;
    *up.hash.lock().unwrap() = hash;
}

async fn next_tip(st: &mut Box<dyn TipStream>) -> TipInfo {
    tokio::time::timeout(Duration::from_secs(5), st.next()).await.expect("a tip within 5 s").unwrap().expect("stream open")
}

#[tokio::test(flavor = "multi_thread")]
async fn tip_push_reaches_every_subscriber_and_costs_the_node_one_poll() {
    let s = fake_srv(None, fast_limits()).await;
    let mut streams = Vec::new();
    let mut eps = Vec::new();
    for _ in 0..5 {
        let c = client_ep(None).await;
        let t = IrohTransport::new(c.clone(), direct_addr(&s.ep), GENESIS);
        let mut st = t.open_tip_stream().await.expect("subscribed");
        // the current tip comes first, with everything a client needs for the limit
        let first = next_tip(&mut st).await;
        assert_eq!((first.height, first.hash, first.difficulty, first.timestamp), (4, [9; 32], 1000, 1_700_000_004));
        assert!(first.server_time > 1_700_000_000);
        streams.push((t, st));
        eps.push(c);
    }
    assert_eq!(s.svc.subscribers(), 5);
    // a new block: pushed to all five (and only once each)
    let before = s.up.calls.load(Ordering::SeqCst);
    let t0 = std::time::Instant::now();
    mine(&s.up, 10);
    for (_, st) in streams.iter_mut() {
        let t = next_tip(st).await;
        assert_eq!((t.height, t.hash, t.timestamp), (5, [10; 32], 1_700_000_005));
    }
    let latency = t0.elapsed();
    assert!(latency < Duration::from_millis(1500), "push latency {latency:?}");
    // the node is asked by the one watcher, not per subscriber: over ~1 s, a handful of get_info calls plus one header
    tokio::time::sleep(Duration::from_secs(1)).await;
    let polls = s.up.calls.load(Ordering::SeqCst) - before;
    assert!(polls <= 20, "one shared watcher at 10 Hz, not 5 subscribers x anything: {polls} node calls");
    // the number of tip frames pushed is one per subscriber per block (plus the first ones)
    assert_eq!(s.svc.stats().tips_pushed.load(Ordering::Relaxed), 10);
    // dropping a stream frees its slot
    drop(streams);
    tokio::time::timeout(Duration::from_secs(5), async { while s.svc.subscribers() > 0 { tokio::time::sleep(Duration::from_millis(50)).await } }).await.expect("subscriptions released");
    for c in eps {
        c.close().await;
    }
    s.router.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn tip_subscription_limits_and_keepalive() {
    let limits = Limits { max_subs_per_endpoint: 1, max_subs_total: 2, ..fast_limits() };
    let s = fake_srv(None, limits).await;
    let c = client_ep(None).await;
    let t = IrohTransport::new(c.clone(), direct_addr(&s.ep), GENESIS);
    let mut a = t.open_tip_stream().await.expect("first subscription");
    next_tip(&mut a).await;
    // the same endpoint may not hold a second one: Busy, not a hang
    match t.open_tip_stream().await {
        Err(TransportError::Protocol(m)) => assert!(m.contains("Busy"), "{m}"),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("per-endpoint limit not enforced"),
    }
    assert_eq!(s.svc.stats().subs_refused.load(Ordering::Relaxed), 1);
    // other endpoints up to the total
    let c2 = client_ep(None).await;
    let t2 = IrohTransport::new(c2.clone(), direct_addr(&s.ep), GENESIS);
    let mut b = t2.open_tip_stream().await.expect("second endpoint");
    next_tip(&mut b).await;
    let c3 = client_ep(None).await;
    let t3 = IrohTransport::new(c3.clone(), direct_addr(&s.ep), GENESIS);
    assert!(t3.open_tip_stream().await.is_err(), "total limit");
    // keepalives flow while nothing changes (raw read: the stream layer swallows them)
    let conn = c3.connect(direct_addr(&s.ep), ALPN).await.unwrap();
    // an unhelloed subscription is refused
    assert_eq!(raw(&conn, &frame(&Request::SubscribeTip)).await.unwrap(), Response::Error(ErrorKind::BadHello));
    drop(a);
    tokio::time::timeout(Duration::from_secs(5), async { while s.svc.subscribers() > 1 { tokio::time::sleep(Duration::from_millis(50)).await } }).await.expect("released");
    let (mut send, mut recv) = {
        let conn = c.connect(direct_addr(&s.ep), ALPN).await.unwrap();
        raw(&conn, &frame(&Request::Hello { minor: MINOR, features: 0, network: GENESIS })).await.unwrap();
        let (mut send, recv) = conn.open_bi().await.unwrap();
        send.write_all(&frame(&Request::SubscribeTip)).await.unwrap();
        send.finish().unwrap();
        std::mem::forget(conn);
        (send, recv)
    };
    let _ = &mut send;
    let mut kinds = Vec::new();
    for _ in 0..3 {
        let body = tokio::time::timeout(Duration::from_secs(3), read_frame(&mut recv, MAX_FRAME)).await.unwrap().unwrap().unwrap();
        kinds.push(decode_body::<Response>(&body).unwrap());
    }
    assert!(matches!(kinds[0], Response::TipPush(_)));
    assert_eq!(kinds[1], Response::Keepalive);
    assert_eq!(kinds[2], Response::Keepalive);
    s.router.shutdown().await.unwrap();
}

/// A minor-1 server: it knows nothing of SubscribeTip and answers an unknown request with `Unsupported`, as the real one did.
#[derive(Clone, Debug)]
struct OldServer;
impl iroh::protocol::ProtocolHandler for OldServer {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), iroh::protocol::AcceptError> {
        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            let Ok(Some(body)) = read_frame(&mut recv, MAX_REQUEST_FRAME).await else { continue };
            let resp = if is_unknown_request_for(&body, 16) {
                Response::Error(ErrorKind::Unsupported)
            } else {
                Response::Hello { minor: 1, features: 0, network: GENESIS, height: 1, tip: [0; 32], relay_url: None }
            };
            let _ = write_frame(&mut send, &resp, MAX_FRAME).await;
            let _ = send.finish();
        }
        Ok(())
    }
}

/// `is_unknown_request` as an older build with `variants` request variants would have computed it.
fn is_unknown_request_for(body: &[u8], variants: u8) -> bool {
    body.first().is_some_and(|b| *b >= variants)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_old_server_answers_unsupported_and_the_client_falls_back() {
    let secret = SecretKey::generate();
    let o = EndpointOptions { secret_key: Some(secret), n0_fallback: false, alpns: vec![ALPN.to_vec()], ..Default::default() };
    let ep = build_endpoint(o).await.unwrap();
    let router = iroh::protocol::Router::builder(ep.clone()).accept(ALPN, OldServer).spawn();
    let c = client_ep(None).await;
    let t = IrohTransport::new(c.clone(), direct_addr(&ep), GENESIS);
    assert!(matches!(t.open_tip_stream().await, Err(TransportError::Unsupported)));
    // the high-level watch learns "nobody pushes" and returns so the caller polls
    let mut list = NodeList::new();
    list.add(from_endpoint_addr(&direct_addr(&ep)), Source::Bootstrap, 0);
    let client = Client::new(list, Arc::new(IrohConnector::new(c.clone(), GENESIS)), Arc::new(SystemEnv));
    let end = tokio::time::timeout(Duration::from_secs(10), client.watch_tip(|_, _| panic!("an old server pushes nothing"))).await.expect("returns");
    assert!(matches!(end, WatchEnd::NoPush));
    router.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_tip_follows_the_chain() {
    let s = fake_srv(None, fast_limits()).await;
    let c = client_ep(None).await;
    let mut list = NodeList::new();
    list.add(from_endpoint_addr(&direct_addr(&s.ep)), Source::Bootstrap, 0);
    let client = Arc::new(Client::new(list, Arc::new(IrohConnector::new(c.clone(), GENESIS)), Arc::new(SystemEnv)));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let cl = client.clone();
    let h = tokio::spawn(async move { cl.watch_tip(move |_, t| { let _ = tx.send(t); }).await });
    let first = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.unwrap().unwrap();
    assert_eq!(first.height, 4);
    mine(&s.up, 11);
    let second = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
    assert_eq!((second.height, second.hash), (5, [11; 32]));
    // the pushed height feeds the node's score
    let h5 = client.with_list(|l| l.records().iter().any(|r| r.height == 5));
    assert!(h5, "tip height recorded");
    h.abort();
    s.router.shutdown().await.unwrap();
}
