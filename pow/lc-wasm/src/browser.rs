//! The wasm-bindgen surface (browser only).
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sm64coin_lc::client::{Connector, Env, Transport, UpstreamTransport};
use sm64coin_lc::nodes::NodeAddr;
use wasm_bindgen::prelude::*;

use crate::core::{Core, anchor_for, build_list, default_rules, https_only, parse_genesis};
use crate::fetch::FetchUpstream;

struct BrowserEnv;
impl Env for BrowserEnv {
    fn now_ms(&self) -> u64 {
        js_sys::Date::now() as u64
    }
    fn rand(&self) -> u64 {
        ((js_sys::Math::random() * 4294967296.0) as u64) << 32 | (js_sys::Math::random() * 4294967296.0) as u64
    }
}

/// HTTP(S) nodes through `fetch`.
struct BrowserConnector {
    http: Mutex<HashMap<String, Arc<dyn Transport>>>,
}

impl Connector for BrowserConnector {
    fn transport(&self, addr: &NodeAddr) -> Option<Arc<dyn Transport>> {
        let NodeAddr::Http { url } = addr;
        let mut m = self.http.lock().unwrap();
        Some(m.entry(url.clone()).or_insert_with(|| Arc::new(UpstreamTransport(FetchUpstream::new(url)))).clone())
    }
}

/// The light client. One per page.
#[wasm_bindgen]
pub struct LightClient {
    core: Core,
}

#[wasm_bindgen]
impl LightClient {
    /// `bootstrap_json`: public-node.json (`{"nodes": [...]}` / `{"url": ...}`; the https entries are the nodes). `genesis`: the chain's genesis block hash (hex; "" or "any": any chain, no
    /// chain check and no verification). `saved_json`: the list a previous visit saved (`list_json()`), or "".
    pub async fn create(bootstrap_json: String, genesis: String, saved_json: String) -> LightClient {
        console_error_panic_hook_free();
        let genesis_bytes = parse_genesis(&genesis);
        let now = (js_sys::Date::now() / 1000.0) as u64;
        let list = build_list(&bootstrap_json, &saved_json, now, https_only);
        let connector = Arc::new(BrowserConnector { http: Default::default() });
        let core = Core::new(list, connector, Arc::new(BrowserEnv), genesis_bytes, anchor_for(genesis_bytes), default_rules());
        LightClient { core }
    }

    /// True when `method` may go through the light-client network (the node RPC allowlist); anything else must use
    /// a node directly.
    pub fn allowed(method: &str) -> bool {
        // a call that carries the view key goes to the one public node, never to a failover pick
        sm64coin_lc::allowlist::rpc_allowed(method) && !sm64coin_lc::allowlist::pinned_only(method)
    }

    /// A JSON-RPC call with failover across the node list. Resolves to the node's whole JSON-RPC answer as text
    /// (`{"result": ..}` or `{"error": ..}`); rejects when no node could answer.
    pub async fn rpc(&self, method: String, params_json: String) -> Result<String, JsValue> {
        self.core.rpc(&method, &params_json).await.map_err(|e| JsValue::from_str(&e))
    }

    /// A submission (`submit_block`, `submit_sm64_proof`) sent to several nodes at once: JSON
    /// `[{node, ok, answer | error}]`.
    pub async fn submit(&self, method: String, params_json: String) -> Result<String, JsValue> {
        self.core.submit(&method, &params_json).await.map_err(|e| JsValue::from_str(&e))
    }

    /// Refreshes the tips and chain-checks new nodes (a node on another chain is banned). Call every ~30 s.
    pub async fn refresh(&self) {
        self.core.refresh().await
    }

    /// Verifies the chain from the compiled-in checkpoint with several nodes; liars are banned. Resolves to the
    /// result as JSON: `{level: "full" | "structural" | "failed" | "none", height, id, quorum, nodes, banned, at}`.
    /// Heavy (it downloads every block since the checkpoint from a few nodes): call at start and every few minutes.
    pub async fn verify(&self) -> String {
        self.core.verify().await.to_string()
    }

    /// Node list with scores, the last verification and the allowed set, as JSON.
    pub fn status(&self) -> String {
        self.core.status().to_string()
    }

    /// The node list to persist (scores and bans survive a reload).
    pub fn list_json(&self) -> String {
        self.core.client.list_json()
    }

    /// Bans a node caught lying (a week).
    pub fn ban_liar(&self, key: &str) {
        self.core.client.ban_liar(key)
    }
}

fn console_error_panic_hook_free() {}
