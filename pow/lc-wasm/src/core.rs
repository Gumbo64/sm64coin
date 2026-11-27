//! The client logic without any JS: failover reads, fan-out submissions, chain probe, header verification
//! and the status report. `lib.rs` wraps it for wasm-bindgen; the tests drive it with fake transports.
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use sm64coin_lc::client::{BULK_TIMEOUT, Client, ClientError, Connector, Env, READ_TIMEOUT};
use sm64coin_lc::nodes::{NodeAddr, NodeList, Source, parse_bootstrap};
use sm64coin_lc::proto::{ErrorKind, Request, Response};
use sm64coin_lc::verify::{Checkpoint, Consensus, CrossCheckOptions, NodeVerdict, Resume, VerifyOptions};

/// Nodes asked by one verification (every one is verified from the anchor; the Pi must not be hammered).
pub const VERIFY_NODES: usize = 3;
/// Nodes that must agree on the verified tip.
pub const VERIFY_QUORUM: usize = 2;
/// Blocks one verification downloads per node at most (a visitor far behind the compiled checkpoint catches up over
/// several checks instead of pulling the whole range from every node at once).
pub const VERIFY_MAX_BLOCKS: u64 = 500;
/// At most this many unprobed nodes are chain-checked per refresh.
const PROBE_PER_REFRESH: usize = 6;

pub struct Core {
    pub client: Arc<Client>,
    env: Arc<dyn Env>,
    /// The network's genesis block hash (None: "any chain", nothing is probed or verified).
    genesis: Option<[u8; 32]>,
    anchor: Option<Checkpoint>,
    rules: Box<dyn Consensus>,
    probed: Mutex<HashSet<String>>,
    last_verify: Mutex<Option<Value>>,
    verifying: Mutex<bool>,
    /// Where the next verification starts (memory only: never read from storage, so nothing a page could have written to
    /// localStorage becomes a trust anchor; a new page load verifies from the compiled checkpoint once).
    resume: Mutex<Option<Resume>>,
}

struct Busy<'a>(&'a Mutex<bool>);
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = false;
    }
}

pub fn parse_genesis(s: &str) -> Option<[u8; 32]> {
    hex::decode(s.trim()).ok()?.try_into().ok()
}

/// The node list: the saved one (scores, bans) topped up with the bootstrap entries. `keep` filters entries the
/// platform cannot reach (iroh nodes when no endpoint could be built).
///
/// The saved list comes from localStorage, which is not trusted: it keeps only records of current bootstrap entries, and
/// only their latency and success history (`NodeList::sanitize_loaded`: no bans, heights or extra nodes from storage).
pub fn build_list(bootstrap_json: &str, saved_json: &str, now: u64, keep: impl Fn(&NodeAddr) -> bool) -> NodeList {
    let boot: Vec<NodeAddr> = parse_bootstrap(bootstrap_json).unwrap_or_default();
    let mut list = NodeList::from_json(saved_json).unwrap_or_else(|_| NodeList::new());
    list.sanitize_loaded(&boot.iter().map(|a| a.key()).collect::<Vec<_>>(), now);
    for a in boot {
        if keep(&a) {
            list.add(a, Source::Bootstrap, now);
        }
    }
    let dead: Vec<String> = list.records().iter().filter(|r| !keep(&r.addr)).map(|r| r.key()).collect();
    for k in dead {
        list.remove(&k);
    }
    list
}

impl Core {
    pub fn new(list: NodeList, connector: Arc<dyn Connector>, env: Arc<dyn Env>, genesis: Option<[u8; 32]>, anchor: Option<Checkpoint>, rules: Box<dyn Consensus>) -> Self {
        Self { client: Arc::new(Client::new(list, connector, env.clone())), env, genesis, anchor, rules, probed: Default::default(), last_verify: Default::default(), verifying: Default::default(), resume: Default::default() }
    }

    fn now_secs(&self) -> u64 {
        self.env.now_ms() / 1000
    }

    /// A JSON-RPC call with failover: the node's whole JSON-RPC answer as text (`result` or `error` inside).
    pub async fn rpc(&self, method: &str, params_json: &str) -> Result<String, String> {
        if sm64coin_lc::allowlist::pinned_only(method) {
            return Err(format!("{method}: not sent to a failover node"));
        }
        let req = Request::JsonRpc { method: method.to_string(), params: params_json.to_string() };
        let timeout = if req.is_bulk() { BULK_TIMEOUT } else { READ_TIMEOUT };
        match self.client.read(&req, timeout).await {
            Ok((_, Response::JsonRpc(text))) => Ok(text),
            Ok((k, other)) => Err(format!("{k}: unexpected answer {other:?}")),
            Err(ClientError::Refused(ErrorKind::Unsupported)) => Err(format!("{method}: not allowed")),
            Err(e) => Err(e.to_string()),
        }
    }

    /// A submission to several nodes at once: `[{node, ok, answer | error}]` as JSON text.
    pub async fn submit(&self, method: &str, params_json: &str) -> Result<String, String> {
        let req = Request::JsonRpc { method: method.to_string(), params: params_json.to_string() };
        let outs = self.client.submit(&req, READ_TIMEOUT).await.map_err(|e| e.to_string())?;
        let v: Vec<Value> = outs
            .into_iter()
            .map(|(node, r)| match r {
                Ok(Response::JsonRpc(text)) => json!({"node": node, "ok": true, "answer": text}),
                Ok(Response::Error(e)) => json!({"node": node, "ok": false, "error": format!("{e:?}")}),
                Ok(other) => json!({"node": node, "ok": false, "error": format!("unexpected answer {other:?}")}),
                Err(e) => json!({"node": node, "ok": false, "error": e}),
            })
            .collect();
        Ok(Value::Array(v).to_string())
    }

    /// Refreshes tips (so lagging nodes are skipped) and chain-checks nodes not yet seen: a node whose genesis block is
    /// not ours is on another chain and is banned.
    pub async fn refresh(&self) {
        self.client.refresh_tips(8, READ_TIMEOUT).await;
        let Some(genesis) = self.genesis else { return };
        let todo: Vec<String> = {
            let probed = self.probed.lock().unwrap();
            let all = self.client.with_list(|l| l.len());
            self.client.pick_keys(all).into_iter().filter(|k| !probed.contains(k)).take(PROBE_PER_REFRESH).collect()
        };
        let req = Request::JsonRpc { method: "get_block_header_by_height".into(), params: r#"{"height":0}"#.into() };
        let futs = todo.iter().map(|k| {
            let req = &req;
            async move { (k.clone(), self.client.ask_node(k, req, READ_TIMEOUT).await) }
        });
        for (k, r) in futures_util::future::join_all(futs).await {
            let Ok(Response::JsonRpc(text)) = r else { continue };
            let hash = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["result"]["block_header"]["hash"].as_str().map(str::to_ascii_lowercase));
            self.probed.lock().unwrap().insert(k.clone());
            if hash.as_deref() != Some(&hex::encode(genesis)) {
                self.client.ban_liar(&k);
            }
        }
    }

    /// Verifies the chain from the compiled-in checkpoint with several nodes (headers, ids, timestamps and, with the
    /// consensus rules, difficulty, frame limit, signatures and proof weight; no Mario replay here). Nodes that serve an
    /// invalid chain are banned; reads and submissions then use only nodes on the verified chain.
    pub async fn verify(&self) -> Value {
        let Some(cp) = &self.anchor else {
            return self.record(json!({"level": "none", "note": "no checkpoint for this network"}));
        };
        {
            let mut busy = self.verifying.lock().unwrap();
            if *busy {
                return self.last_verify.lock().unwrap().clone().unwrap_or(json!({"level": "pending"}));
            }
            *busy = true;
        }
        let _guard = Busy(&self.verifying);
        let opts = CrossCheckOptions {
            nodes: VERIFY_NODES,
            quorum: VERIFY_QUORUM,
            verify: VerifyOptions { now_secs: Some(self.now_secs()), keep_blocks: 4, max_blocks: Some(VERIFY_MAX_BLOCKS), ..Default::default() },
            ..Default::default()
        };
        let resume = self.resume.lock().unwrap().clone();
        let res = sm64coin_lc::verify::sync_resume(&self.client, cp, resume.as_ref(), self.rules.as_ref(), None, &opts, &|| false).await;
        let v = match res {
            Ok(o) => {
                if o.resume.is_some() {
                    *self.resume.lock().unwrap() = o.resume.clone();
                }
                let ok_keys: Vec<String> = o.nodes.iter().filter(|(_, v)| matches!(v, NodeVerdict::Agrees | NodeVerdict::Behind)).map(|(k, _)| k.clone()).collect();
                self.client.set_allowed(Some(ok_keys));
                let level = format!("{:?}", o.best.level).to_lowercase();
                json!({
                    "level": level,
                    "height": o.best.tip.height,
                    "id": hex::encode(o.best.tip.id),
                    "quorum": o.quorum_met,
                    "anchor": cp.height,
                    "partial": o.partial,
                    "from": o.best.anchor_height,
                    "reanchored": o.reanchored,
                    "nodes": o.nodes.iter().map(|(k, v)| json!([k, verdict_str(v)])).collect::<Vec<_>>(),
                    "banned": o.banned,
                })
            }
            Err(e) => {
                // nothing verified: reads are not restricted to an old verified set (that could leave no node at all)
                self.client.set_allowed(None);
                json!({"level": "failed", "error": e.to_string(), "anchor": cp.height})
            }
        };
        self.record(v)
    }

    fn record(&self, mut v: Value) -> Value {
        v["at"] = json!(self.now_secs());
        *self.last_verify.lock().unwrap() = Some(v.clone());
        v
    }

    /// Node list, scores and the last verification, for the Network page.
    pub fn status(&self) -> Value {
        let list: Value = serde_json::from_str(&self.client.list_json()).unwrap_or(Value::Null);
        json!({"list": list, "verify": self.last_verify.lock().unwrap().clone(), "allowed": self.client.allowed(), "now": self.now_secs()})
    }
}

fn verdict_str(v: &NodeVerdict) -> String {
    match v {
        NodeVerdict::Agrees => "agrees".into(),
        NodeVerdict::Behind => "behind".into(),
        NodeVerdict::Forked => "forked".into(),
        NodeVerdict::Unreachable(e) => format!("unreachable: {e}"),
        NodeVerdict::Liar(e) => format!("liar: {e}"),
    }
}

/// The rules for a network: the real ones when built with `consensus`, else structural only.
pub fn default_rules() -> Box<dyn Consensus> {
    #[cfg(feature = "consensus")]
    {
        Box::new(sm64coin_lc::consensus::Sm64Consensus)
    }
    #[cfg(not(feature = "consensus"))]
    {
        Box::new(sm64coin_lc::verify::StructuralOnly)
    }
}

/// The compiled-in anchor when `genesis` is the mainnet genesis (any other chain has none).
pub fn anchor_for(genesis: Option<[u8; 32]>) -> Option<Checkpoint> {
    let g = genesis?;
    let cps = sm64coin_lc::checkpoints::mainnet();
    (cps.first()?.id == g).then(|| cps.last().cloned()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sm64coin_lc::client::{Transport, TransportError};
    use std::time::Duration;

    struct TestEnv;
    impl Env for TestEnv {
        fn now_ms(&self) -> u64 {
            1_000_000
        }
        fn rand(&self) -> u64 {
            7
        }
    }

    /// A node: answers `get_info`-ish JSON-RPC with its name, or fails, or has another genesis.
    struct Fake {
        name: &'static str,
        fail: bool,
        genesis: &'static str,
    }

    #[async_trait::async_trait]
    impl Transport for Fake {
        async fn request(&self, req: &Request, _t: Duration) -> Result<Response, TransportError> {
            if self.fail {
                return Err(TransportError::Connect("down".into()));
            }
            match req {
                Request::JsonRpc { method, .. } if method == "get_block_header_by_height" => {
                    Ok(Response::JsonRpc(format!(r#"{{"result":{{"block_header":{{"hash":"{}"}}}}}}"#, self.genesis)))
                }
                Request::JsonRpc { method, .. } => Ok(Response::JsonRpc(format!(r#"{{"result":{{"node":"{}","method":"{method}"}}}}"#, self.name))),
                Request::GetTip => Ok(Response::Tip(sm64coin_lc::proto::Tip { height: 10, hash: [1; 32], cumulative_difficulty: 10 })),
                _ => Ok(Response::Error(ErrorKind::Unsupported)),
            }
        }
    }

    struct Conn(Vec<(&'static str, Arc<Fake>)>);
    impl Connector for Conn {
        fn transport(&self, addr: &NodeAddr) -> Option<Arc<dyn Transport>> {
            let NodeAddr::Http { url } = addr else { return None };
            self.0.iter().find(|(u, _)| url.contains(u)).map(|(_, f)| f.clone() as Arc<dyn Transport>)
        }
    }

    const G: &str = "6b7164fc12bd735bfe29caf87615ec62ca318f78539afae11a13a244eb080482";

    fn core(nodes: Vec<(&'static str, bool, &'static str)>) -> Core {
        let boot = format!(r#"{{"nodes":[{}]}}"#, nodes.iter().map(|(n, ..)| format!("\"https://{n}\"")).collect::<Vec<_>>().join(","));
        let list = build_list(&boot, "", 1000, |_| true);
        let conn = Conn(nodes.iter().map(|(n, fail, g)| (*n, Arc::new(Fake { name: n, fail: *fail, genesis: g }))).collect());
        Core::new(list, Arc::new(conn), Arc::new(TestEnv), parse_genesis(G), None, default_rules())
    }

    #[test]
    fn rpc_fails_over_to_a_node_that_answers() {
        let c = core(vec![("a.example", true, G), ("b.example", false, G)]);
        for _ in 0..3 {
            let a = futures_executor::block_on(c.rpc("get_info", "{}")).unwrap();
            assert!(a.contains("b.example"), "{a}");
        }
        let st = c.status();
        let recs = st["list"]["records"].as_array().unwrap();
        let a = recs.iter().find(|r| r["addr"]["Http"]["url"] == "https://a.example").unwrap();
        assert!(a["strikes"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn rpc_reports_when_every_node_is_down_and_when_a_method_is_not_allowed() {
        let c = core(vec![("a.example", true, G)]);
        assert!(futures_executor::block_on(c.rpc("get_info", "{}")).is_err());
        let c = core(vec![("a.example", false, G)]);
        // the fake does not enforce the allowlist; the real transports do (exec::validate): check the lc rule itself
        assert!(!sm64coin_lc::allowlist::rpc_allowed("stop_daemon"));
        assert!(sm64coin_lc::allowlist::rpc_allowed("get_info"));
        assert!(futures_executor::block_on(c.rpc("get_info", "{}")).is_ok());
        // the view-key call never goes to a failover pick
        assert!(futures_executor::block_on(c.rpc("sm64_board_join", "{}")).is_err());
    }

    #[test]
    fn submit_goes_to_every_node_and_reports_each() {
        let c = core(vec![("a.example", true, G), ("b.example", false, G), ("c.example", false, G)]);
        let out: Value = serde_json::from_str(&futures_executor::block_on(c.submit("submit_block", r#"["00"]"#)).unwrap()).unwrap();
        let v = out.as_array().unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v.iter().filter(|r| r["ok"] == true).count(), 2);
        assert_eq!(v.iter().filter(|r| r["ok"] == false).count(), 1);
    }

    #[test]
    fn refresh_bans_a_node_on_another_chain() {
        let c = core(vec![("good.example", false, G), ("old.example", false, "00".repeat(32).leak())]);
        futures_executor::block_on(c.refresh());
        let banned = |n: &str| c.client.with_list(|l| l.get(&format!("http:https://{n}")).map(|r| r.is_banned(1000)).unwrap());
        assert!(!banned("good.example"));
        assert!(banned("old.example"));
        // and reads no longer go there
        for _ in 0..5 {
            assert!(futures_executor::block_on(c.rpc("get_info", "{}")).unwrap().contains("good.example"));
        }
    }

    #[test]
    fn list_is_saved_and_restored_and_unreachable_kinds_dropped() {
        let id = "ab".repeat(32);
        let boot = format!(r#"{{"nodes":["https://a.example","iroh:{id}@https://relay.example"]}}"#);
        let with = build_list(&boot, "", 1, |_| true);
        assert_eq!(with.len(), 2);
        let without = build_list(&boot, &with.to_json(), 2, |a| matches!(a, NodeAddr::Http { .. }));
        assert_eq!(without.len(), 1, "an iroh node is dropped when there is no endpoint, even from the saved list");
        assert!(build_list(r#"{"url":"https://eboy.au"}"#, "garbage", 1, |_| true).len() == 1);
    }

    #[test]
    fn a_poisoned_saved_list_cannot_ban_the_bootstrap_nodes_or_add_its_own() {
        let boot = r#"{"nodes":["https://a.example","https://b.example"]}"#;
        let mut evil = NodeList::new();
        evil.add(NodeAddr::Http { url: "https://a.example".into() }, Source::Manual, 1);
        evil.ban_liar("http:https://a.example", 1);
        evil.add(NodeAddr::Http { url: "https://evil.example".into() }, Source::Manual, 1);
        evil.record_ok("http:https://evil.example", 1.0, Some(1 << 40), 1);
        let list = build_list(boot, &evil.to_json(), 1000, |_| true);
        assert_eq!(list.len(), 2);
        assert!(list.get("http:https://evil.example").is_none());
        assert!(!list.get("http:https://a.example").unwrap().is_banned(1000));
        assert_eq!(list.get("http:https://a.example").unwrap().source, Source::Bootstrap);
    }

    #[test]
    fn anchor_only_for_the_mainnet_genesis() {
        assert!(anchor_for(parse_genesis(G)).is_some() == (sm64coin_lc::checkpoints::mainnet()[0].id == parse_genesis(G).unwrap()));
        assert!(anchor_for(None).is_none());
        assert!(anchor_for(Some([9; 32])).is_none());
    }
}

/// A pushed tip as the JSON the page reads (`difficulty` and `cumulative_difficulty` as decimal strings: they can exceed 2^53).
pub fn tip_json(node: &str, t: &sm64coin_lc::proto::TipInfo) -> String {
    json!({
        "node": node,
        "height": t.height,
        "hash": hex::encode(t.hash),
        "timestamp": t.timestamp,
        "cumulative_difficulty": t.cumulative_difficulty.to_string(),
        "difficulty": t.difficulty.to_string(),
        "server_time": t.server_time,
    })
    .to_string()
}
