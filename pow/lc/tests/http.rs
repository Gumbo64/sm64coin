//! Over HTTP(S): [`UpstreamTransport`] against a fake node RPC (the allowlist, range checks, answer shapes) and against an
//! isolated offline `sm64coind` (the end-to-end one, skipped when the app is not installed). Nothing here touches the real
//! network or chain.
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use sm64coin_lc::client::*;
use sm64coin_lc::exec::{UpError, Upstream};
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::proto::*;

const T: Duration = Duration::from_secs(20);

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

/// The fake node, shared so a test can read its call count.
struct Shared(Arc<FakeUp>);

fn transport(up: &Arc<FakeUp>) -> UpstreamTransport<Shared> {
    UpstreamTransport(Shared(up.clone()))
}

#[async_trait::async_trait]
impl Upstream for Shared {
    async fn json_rpc(&self, method: &str, params: Value, t: Duration) -> Result<Value, UpError> {
        self.0.json_rpc(method, params, t).await
    }
    async fn post(&self, p: &str, c: &str, body: Vec<u8>, t: Duration) -> Result<Vec<u8>, UpError> {
        self.0.post(p, c, body, t).await
    }
}

#[tokio::test]
async fn requests_are_answered_from_the_node_and_range_checked() {
    let up = Arc::new(FakeUp::default());
    *up.height.lock().unwrap() = 4;
    *up.hash.lock().unwrap() = 9;
    let t = transport(&up);
    match t.request(&Request::GetTip, T).await.unwrap() {
        Response::Tip(tip) => assert_eq!((tip.height, tip.hash, tip.cumulative_difficulty), (4, [9; 32], 42)),
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
    let before = up.calls.load(Ordering::SeqCst);
    assert!(matches!(t.request(&Request::GetHeaders { from: 0, count: 5000 }, T).await.unwrap(), Response::Error(ErrorKind::Malformed)));
    assert_eq!(up.calls.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn passthrough_json_rpc_and_paths_with_the_allowlist() {
    let up = Arc::new(FakeUp::default());
    let t = transport(&up);
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
    let calls = up.calls.load(Ordering::SeqCst);
    // not on the allowlist: refused without touching the node
    for m in ["stop_daemon", "set_bans", "prune_blockchain", "get_connections"] {
        assert_eq!(t.request(&Request::JsonRpc { method: m.into(), params: "{}".into() }, T).await.unwrap(), Response::Error(ErrorKind::Unsupported), "{m}");
    }
    for p in ["/stop_daemon", "/get_transaction_pool", "/get_peer_list", "/start_mining", "/../json_rpc"] {
        assert_eq!(t.request(&Request::Path { path: p.into(), body: vec![] }, T).await.unwrap(), Response::Error(ErrorKind::Unsupported), "{p}");
    }
    assert_eq!(t.request(&Request::JsonRpc { method: "get_info".into(), params: "not json".into() }, T).await.unwrap(), Response::Error(ErrorKind::Malformed));
    assert_eq!(up.calls.load(Ordering::SeqCst), calls);
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
    static N: AtomicUsize = AtomicUsize::new(0);
    let data = std::env::temp_dir().join(format!("lc-test-node-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
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

async fn genesis(up: &HttpUpstream) -> [u8; 32] {
    let mut genesis = Err(String::new());
    for _ in 0..40 {
        genesis = sm64coin_lc::exec::genesis_hash(up, Duration::from_secs(2)).await;
        if genesis.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    genesis.expect("the offline node answers")
}

#[tokio::test(flavor = "multi_thread")]
async fn end_to_end_against_an_isolated_offline_node() {
    let Some(d) = start_daemon() else { return };
    let up = HttpUpstream::new(&format!("http://127.0.0.1:{}", d.rpc)).unwrap();
    let network = genesis(&up).await;
    let t = UpstreamTransport(up);

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
}

/// A real node behind the tip service: the pushed tip is the node's (block, timestamp, difficulty), and the limit a client computes
/// from it with the Rust rules is the number the node itself reports for a block found now.
#[cfg(feature = "consensus")]
#[tokio::test(flavor = "multi_thread")]
async fn tip_push_from_a_real_node_gives_the_nodes_own_limit() {
    use sm64coin_lc::sse::{SseConfig, serve};
    use sm64coin_lc::tipfeed::{TipFeed, TipFeedConfig};
    let Some(d) = start_daemon() else { return };
    let up = Arc::new(HttpUpstream::new(&format!("http://127.0.0.1:{}", d.rpc)).unwrap());
    let network = genesis(&up).await;
    let feed = TipFeed::new(up.clone(), TipFeedConfig { poll: Duration::from_millis(100), idle_poll: Duration::from_millis(100), ..Default::default() });
    feed.refresh().await;
    tokio::spawn(feed.clone().run(|| false));
    let (addr, _task) = serve(feed, SseConfig::default()).await.unwrap();
    let t = UpstreamTransport(HttpUpstream::new(&format!("http://{addr}")).unwrap());
    let mut st = t.open_tip_stream().await.expect("a real node's service pushes");
    let tip = tokio::time::timeout(Duration::from_secs(5), st.next()).await.expect("a tip within 5 s").unwrap().expect("stream open");
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
}
