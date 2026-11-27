//! The wasm-bindgen surface (browser only).
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sm64coin_lc::client::{Connector, Env, IrohConnector, Transport, UpstreamTransport};
use sm64coin_lc::nodes::NodeAddr;
use wasm_bindgen::prelude::*;

use crate::core::{Core, anchor_for, build_list, default_rules, parse_genesis};
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

/// HTTP(S) nodes through `fetch`, iroh nodes through the endpoint (when one could be built).
struct BrowserConnector {
    http: Mutex<HashMap<String, Arc<dyn Transport>>>,
    iroh: Option<IrohConnector>,
}

impl Connector for BrowserConnector {
    fn transport(&self, addr: &NodeAddr) -> Option<Arc<dyn Transport>> {
        match addr {
            NodeAddr::Http { url } => {
                let mut m = self.http.lock().unwrap();
                Some(m.entry(url.clone()).or_insert_with(|| Arc::new(UpstreamTransport(FetchUpstream::new(url)))).clone())
            }
            NodeAddr::Iroh { .. } => self.iroh.as_ref()?.transport(addr),
        }
    }
}

/// Builds the browser's iroh endpoint (relay-only; nothing is published; the relays are the bootstrap entries' own,
/// n0's public ones only as a fallback after a few seconds). None when it cannot be built.
async fn iroh_connector(relays: Vec<String>, network: [u8; 32]) -> Option<IrohConnector> {
    use sm64coin_lc::endpoint::{Discovery, EndpointOptions, build_endpoint};
    let relays = relays.iter().filter_map(|r| r.parse::<iroh::RelayUrl>().ok()).collect();
    let opts = EndpointOptions { relays, discovery: Discovery::None, ..Default::default() };
    match build_endpoint(opts).await {
        Ok(ep) => Some(IrohConnector::new(ep, network)),
        Err(e) => {
            web_sys_log(&format!("lightnet: no iroh endpoint ({e}); HTTPS nodes only"));
            None
        }
    }
}

fn web_sys_log(s: &str) {
    let _ = js_sys::Reflect::get(&js_sys::global(), &"console".into()).and_then(|c| {
        let f: js_sys::Function = js_sys::Reflect::get(&c, &"warn".into())?.into();
        f.call1(&c, &s.into())
    });
}

/// The light client. One per page.
#[wasm_bindgen]
pub struct LightClient {
    core: Core,
    iroh: bool,
}

#[wasm_bindgen]
impl LightClient {
    /// `bootstrap_json`: public-node.json (`{"nodes": [...]}` / `{"url": ...}`; entries are https URLs or
    /// `iroh:<endpoint id>@<relay url>`). `genesis`: the chain's genesis block hash (hex; "" or "any": any chain, no
    /// chain check and no verification). `saved_json`: the list a previous visit saved (`list_json()`), or "".
    pub async fn create(bootstrap_json: String, genesis: String, saved_json: String) -> LightClient {
        console_error_panic_hook_free();
        let genesis_bytes = parse_genesis(&genesis);
        let wants_iroh = sm64coin_lc::nodes::parse_bootstrap(&bootstrap_json).unwrap_or_default().iter().any(|a| matches!(a, NodeAddr::Iroh { .. }))
            || sm64coin_lc::nodes::NodeList::from_json(&saved_json).map(|l| l.records().iter().any(|r| matches!(r.addr, NodeAddr::Iroh { .. }))).unwrap_or(false);
        let mut iroh = None;
        if wants_iroh {
            if let Some(net) = genesis_bytes {
                let mut relays: Vec<String> = Vec::new();
                for a in sm64coin_lc::nodes::parse_bootstrap(&bootstrap_json).unwrap_or_default() {
                    if let NodeAddr::Iroh { relay_url: Some(r), .. } = a {
                        if !relays.contains(&r) {
                            relays.push(r);
                        }
                    }
                }
                iroh = iroh_connector(relays, net).await;
            }
        }
        let has_iroh = iroh.is_some();
        let now = (js_sys::Date::now() / 1000.0) as u64;
        let list = build_list(&bootstrap_json, &saved_json, now, |a| has_iroh || !matches!(a, NodeAddr::Iroh { .. }));
        let connector = Arc::new(BrowserConnector { http: Default::default(), iroh });
        let core = Core::new(list, connector, Arc::new(BrowserEnv), genesis_bytes, anchor_for(genesis_bytes), default_rules());
        LightClient { core, iroh: has_iroh }
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
        let mut v = self.core.status();
        v["iroh"] = self.iroh.into();
        v.to_string()
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
