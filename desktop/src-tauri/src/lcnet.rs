//! The app's light-client network: `sm64coin_lc::client::Client` over a scored `NodeList` (compiled-in bootstrap nodes, the node
//! the user confirmed, learned ones kept in `nodes.json`), with two transports (HTTP(S) JSON-RPC and iroh) and failover between
//! all of them. `remote.rs` (the page's `remote_rpc`) and `walletproxy.rs` (the wallet's and the Android miner's daemon) both
//! go through here, so there is one list, one set of scores and one "which node is in use".
//!
//! Which calls fail over over which transport: every allowlisted call. A request the light-client protocol has a typed message
//! for (the wallet's binary calls, `get_sm64_cycle`, `get_fee_estimate`, `get_block_headers_range`, a plain `submit_block` ...)
//! is that message; every other allowlisted JSON-RPC call (`get_info`, `get_sm64_state`, a template with the miner's `sm64_*`
//! extras, `get_block` by hash ...) is `Request::JsonRpc` and every other wallet path (`/get_transactions`, `/is_key_image_spent`,
//! `/get_o_indexes.bin` ...) is `Request::Path`; all of them go to any node, HTTP or iroh, and submissions go to three nodes at
//! once. A node too old for the passthrough requests answers `Unsupported` and the client moves on to the next.
//!
//! Verification (lcverify.rs): on startup and on each new tip the chain is checked from the compiled-in checkpoint across up to
//! five nodes; afterwards reads use only the nodes on the verified best branch, and nodes that lied are banned.
use serde_json::{Value, json};
use sm64coin_lc::allowlist;
use sm64coin_lc::client::{BULK_TIMEOUT, Client, ClientError, Connector, Env, READ_TIMEOUT, SystemEnv, Transport, UpstreamTransport};
use sm64coin_lc::endpoint::{Discovery, EndpointOptions, build_endpoint};
use sm64coin_lc::exec::Upstream;
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::nodes::{NodeAddr, NodeList, Source};
use sm64coin_lc::proto::{ErrorKind, Request, Response, WalletBin, ALPN};
use crate::lcverify::{Verification, Verifier, checkpoint_for};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// v0.5 mainnet's genesis block hash (web/src/lib/nodechain.js `MAINNET_GENESIS`): the `network` of the iroh `Hello`.
pub const MAINNET_GENESIS: &str = "6b7164fc12bd735bfe29caf87615ec62ca318f78539afae11a13a244eb080482";

/// A node the last call went to.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Used {
    /// The node's key (`http:https://host` / `iroh:<endpoint id>`).
    pub node: String,
    /// `https`, `http` or `iroh`.
    pub transport: &'static str,
    pub method: String,
    pub at_ms: u64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct NodeInfo {
    pub key: String,
    pub transport: &'static str,
    pub source: String,
    pub ok_rate: f64,
    pub latency_ms: f64,
    pub height: u64,
    pub banned: bool,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct NetStatus {
    /// The loopback daemon the wallet and the native miner use (`http://127.0.0.1:port`), once the proxy runs.
    pub proxy: Option<String>,
    pub used: Option<Used>,
    /// This app's iroh endpoint id, once the endpoint is up (None: HTTP nodes only so far).
    pub iroh: Option<String>,
    pub nodes: Vec<NodeInfo>,
    /// What the verifier found (see lcverify.rs): `verified (n nodes agree)`, `conflict`, `unverified`.
    pub verify: Verification,
}

pub fn transport_of(addr: &NodeAddr) -> &'static str {
    match addr {
        NodeAddr::Http { url } if url.starts_with("https://") => "https",
        NodeAddr::Http { .. } => "http",
        NodeAddr::Iroh { .. } => "iroh",
    }
}

/// Builds the transport of a node: HTTP always, iroh once the endpoint is up (before that an iroh node is skipped: failover).
struct AppConnector {
    iroh: RwLock<Option<Arc<dyn Connector>>>,
    http: Mutex<HashMap<String, (HttpUpstream, Arc<dyn Transport>)>>,
}

impl AppConnector {
    fn http_entry(&self, url: &str) -> Option<(HttpUpstream, Arc<dyn Transport>)> {
        let mut m = self.http.lock().unwrap();
        if let Some(e) = m.get(url) {
            return Some(e.clone());
        }
        let up = HttpUpstream::new(url).ok()?;
        let e: (HttpUpstream, Arc<dyn Transport>) = (up.clone(), Arc::new(UpstreamTransport(up)));
        m.insert(url.to_string(), e.clone());
        Some(e)
    }
}

impl Connector for AppConnector {
    fn transport(&self, addr: &NodeAddr) -> Option<Arc<dyn Transport>> {
        match addr {
            NodeAddr::Http { url } => self.http_entry(url).map(|e| e.1),
            NodeAddr::Iroh { .. } => self.iroh.read().unwrap().as_ref()?.transport(addr),
        }
    }
}

/// Supplies the ROM bytes (desktop: from the app's data dir) for the replay of the current cycle.
pub type RomFinder = Arc<dyn Fn() -> Option<Vec<u8>> + Send + Sync>;

/// Where the last verification's resume point is kept (next to `nodes.json`): the next start checks only newer blocks.
fn verified_file(nodes_file: &std::path::Path) -> PathBuf {
    nodes_file.with_file_name("lc-verified.json")
}

/// The chain this app follows: mainnet, or `SM64COIN_GENESIS` (tests, closed networks).
pub fn network_genesis() -> [u8; 32] {
    hex::decode(std::env::var("SM64COIN_GENESIS").unwrap_or_else(|_| MAINNET_GENESIS.into())).ok().and_then(|b| <[u8; 32]>::try_from(b).ok()).unwrap_or([0; 32])
}

pub struct Net {
    client: Client,
    conn: Arc<AppConnector>,
    env: SystemEnv,
    nodes_file: Option<PathBuf>,
    saved_json: Mutex<String>,
    used: Mutex<Option<Used>>,
    iroh_id: Mutex<Option<String>>,
    proxy: Mutex<Option<String>>,
    verifier: Arc<Verifier>,
    /// Desktop: where to get the ROM for the background replay (None: header layer only).
    rom_finder: Mutex<Option<RomFinder>>,
    /// The chain's genesis block hash (the iroh `Hello` network and the verifier's anchor).
    network: [u8; 32],
    /// Timeouts of a read and of a bulk read (the tests shorten them).
    pub read_timeout: Duration,
    pub bulk_timeout: Duration,
}

fn now_secs() -> u64 {
    SystemEnv.now_ms() / 1000
}

impl Net {
    /// `bootstrap`: the compiled-in nodes; `saved`: the https node the user confirmed (kept above discovered ones);
    /// `nodes_file`: where learned nodes persist.
    pub fn new(bootstrap: &[NodeAddr], saved: Option<&str>, nodes_file: Option<PathBuf>) -> Arc<Net> {
        let now = now_secs();
        let mut list = nodes_file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|s| NodeList::from_json(&s).ok()).unwrap_or_default();
        // Bootstrap and manual records come from this build and the saved node, not from the file (a node dropped from a
        // release's list must not live on in an old nodes.json).
        let stale: Vec<String> = list.records().iter().filter(|r| matches!(r.source, Source::Bootstrap | Source::Manual)).map(|r| r.key()).collect();
        for k in stale {
            list.remove(&k);
        }
        for a in bootstrap {
            list.add(a.clone(), Source::Bootstrap, now);
        }
        if let Some(s) = saved {
            list.add(NodeAddr::Http { url: s.to_string() }, Source::Manual, now);
        }
        let conn = Arc::new(AppConnector { iroh: RwLock::new(None), http: Mutex::new(HashMap::new()) });
        let verifier = Arc::new(Verifier::default());
        // the stored resume point (the app's own data dir; used only if it descends from this release's checkpoint)
        let stored = nodes_file.as_ref().and_then(|f| std::fs::read_to_string(verified_file(f)).ok()).and_then(|s| sm64coin_lc::verify::Resume::from_json(&s).ok());
        if let Some(r) = stored.filter(|r| r.fits(&checkpoint_for(&network_genesis()))) {
            verifier.set_resume(Some(r));
        }
        Arc::new(Net {
            client: Client::new(list, conn.clone(), Arc::new(SystemEnv)),
            conn,
            env: SystemEnv,
            nodes_file,
            saved_json: Mutex::new(String::new()),
            used: Mutex::new(None),
            iroh_id: Mutex::new(None),
            proxy: Mutex::new(None),
            verifier,
            rom_finder: Mutex::new(None),
            network: network_genesis(),
            read_timeout: READ_TIMEOUT,
            bulk_timeout: BULK_TIMEOUT,
        })
    }

    /// The user's confirmed node changed (or was forgotten).
    pub fn set_saved(&self, old: Option<&str>, new: Option<&str>) {
        let now = now_secs();
        self.client.with_list(|l| {
            if let Some(o) = old {
                l.remove(&NodeAddr::Http { url: o.to_string() }.key());
            }
            if let Some(n) = new {
                l.add(NodeAddr::Http { url: n.to_string() }, Source::Manual, now);
            }
        });
    }

    pub fn set_proxy(&self, url: &str) {
        *self.proxy.lock().unwrap() = Some(url.to_string());
    }

    /// Brings up the iroh endpoint; from then on iroh nodes are usable. `network` = the genesis block hash.
    pub async fn start_iroh(&self, opts: EndpointOptions, network: [u8; 32]) -> Result<String, String> {
        let mut opts = opts;
        if !opts.alpns.contains(&ALPN.to_vec()) {
            opts.alpns.push(ALPN.to_vec());
        }
        let ep = build_endpoint(opts).await.map_err(|e| e.to_string())?;
        let id = ep.id().to_string();
        *self.conn.iroh.write().unwrap() = Some(Arc::new(sm64coin_lc::client::IrohConnector::new(ep, network)));
        *self.iroh_id.lock().unwrap() = Some(id.clone());
        Ok(id)
    }

    /// Persists the node list (when it changed).
    pub fn persist(&self) {
        let Some(f) = &self.nodes_file else { return };
        if let Some(r) = self.verifier.resume() {
            let _ = std::fs::write(verified_file(f), r.to_json());
        }
        let json = self.client.list_json();
        let mut last = self.saved_json.lock().unwrap();
        if *last != json && std::fs::write(f, &json).is_ok() {
            *last = json;
        }
    }

    /// Asks up to three nodes for their lists of light-client servers (iroh nodes answer) and learns the verified ones.
    pub async fn gossip(&self) -> usize {
        let mut learned = 0;
        for _ in 0..3 {
            match self.client.read(&Request::GetNodes { max: sm64coin_lc::proto::MAX_NODES }, self.read_timeout).await {
                Ok((_, Response::Nodes(v))) => learned += self.client.learn(&v),
                Err(ClientError::NoNodes) => break,
                _ => {}
            }
        }
        learned
    }

    /// The relays of the iroh nodes we know (compiled-in `iroh:<id>@<relay>` entries, the user's, learned ones): the endpoint's
    /// home relay is one of them, so a node reachable only through its relay is dialed without any lookup service.
    fn known_relays(&self) -> Vec<iroh::RelayUrl> {
        let mut v: Vec<iroh::RelayUrl> = vec![];
        self.client.with_list(|l| {
            for r in l.records() {
                if let NodeAddr::Iroh { relay_url: Some(u), .. } = &r.addr {
                    if let Ok(u) = u.parse::<iroh::RelayUrl>() {
                        if !v.contains(&u) && v.len() < 4 {
                            v.push(u);
                        }
                    }
                }
            }
        });
        v
    }

    /// Background work of the app: brings the iroh endpoint up (with retries), then verifies the chain (on startup and on every
    /// new tip), every minute persists the list and every ten minutes learns more nodes. Clients publish nothing and look nothing
    /// up (discovery `none`, see `app_discovery`): they dial nodes by id and relay URL.
    pub fn spawn_background(self: &Arc<Self>) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let opts = EndpointOptions { discovery: app_discovery(), relays: me.known_relays(), ..Default::default() };
            let mut tries = 0;
            while me.start_iroh(opts.clone(), me.network).await.is_err() {
                tries += 1;
                if tries > 5 {
                    return me.maintain(false).await; // HTTP nodes only
                }
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            me.maintain(true).await;
        });
    }

    async fn maintain(self: &Arc<Self>, iroh: bool) {
        let mut n = 0u64;
        let mut last_verify: Option<std::time::Instant> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(if n == 0 { 2 } else { 5 })).await;
            if iroh && n % 120 == 0 {
                self.gossip().await;
            }
            if n % 12 == 0 {
                self.persist();
            }
            if n % 4 == 0 {
                // verify: the first time, when the tip moved, after an unverified result (every minute), and at least every ten minutes
                let v = self.verifier.get();
                let tip = self.client.read(&Request::GetTip, self.read_timeout).await.ok().and_then(|(_, r)| if let Response::Tip(t) = r { Some(t) } else { None });
                let moved = match (&tip, v.height) {
                    (Some(t), Some(h)) => t.height > h || (t.height == h && Some(hex::encode(t.hash)) != v.tip),
                    (Some(_), None) => true,
                    (None, _) => false,
                };
                let since = last_verify.map(|t| t.elapsed());
                // no verified node answers any more: check again soon (the check asks any node, and either finds the new verified
                // set or lifts the restriction), so the restriction never leaves the app without a node for long
                let stranded = tip.is_none() && v.state != "unverified";
                let due = match since {
                    None => true,
                    Some(e) => {
                        e > Duration::from_secs(600)
                            || (moved && e > Duration::from_secs(15))
                            || ((v.state == "unverified" || stranded) && e > Duration::from_secs(30))
                            || (v.state == "checking" && e > Duration::from_secs(5))
                    }
                };
                if due {
                    last_verify = Some(std::time::Instant::now());
                    let _ = self.verify_now(None).await;
                    self.persist();
                }
            }
            n += 1;
        }
    }

    fn note(&self, key: &str, method: &str) {
        let transport = if key.starts_with("iroh:") { "iroh" } else if key.starts_with("http:https://") { "https" } else { "http" };
        *self.used.lock().unwrap() = Some(Used { node: key.to_string(), transport, method: method.to_string(), at_ms: self.env.now_ms() });
    }

    pub fn status(&self) -> NetStatus {
        let now = now_secs();
        let nodes = self.client.with_list(|l| {
            l.records()
                .iter()
                .map(|r| NodeInfo { key: r.key(), transport: transport_of(&r.addr), source: format!("{:?}", r.source), ok_rate: r.ok_rate, latency_ms: r.ewma_latency_ms, height: r.height, banned: r.is_banned(now) })
                .collect()
        });
        NetStatus { proxy: self.proxy.lock().unwrap().clone(), used: self.used.lock().unwrap().clone(), iroh: self.iroh_id.lock().unwrap().clone(), nodes, verify: self.verifier.get() }
    }

    /// Desktop: turns the background replay of the current cycle on.
    #[cfg_attr(mobile, allow(dead_code))]
    pub fn enable_replay(&self, rom: RomFinder) {
        *self.rom_finder.lock().unwrap() = Some(rom);
        // the replay needs the current cycle's blocks, which a resume point read from disk does not carry: the first check of
        // this run goes from the checkpoint (later ones resume from memory, which keeps them)
        if self.verifier.resume().is_some_and(|r| r.raw.is_empty()) {
            self.verifier.set_resume(None);
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn verification(&self) -> Verification {
        self.verifier.get()
    }

    /// Header-verifies the chain from `cp` (the release's checkpoint when None) across up to five nodes, restricts reads to the
    /// verified branch's nodes, bans liars, and (desktop, with a ROM) replays the current cycle in the background. A branch that
    /// fails its replay bans its servers and the check runs again (at most three times).
    pub async fn verify_now(self: &Arc<Self>, cp: Option<sm64coin_lc::verify::Checkpoint>) -> Result<(), String> {
        let cp = cp.unwrap_or_else(|| checkpoint_for(&self.network));
        let rom = self.rom_finder.lock().unwrap().clone();
        let out = self.verifier.check(&self.client, &cp, rom.is_some()).await?;
        #[cfg(not(mobile))]
        if let Some(rom) = rom {
            // The replay runs behind the verdict: reads already use the verified nodes; a failed replay re-runs the check.
            let me = self.clone();
            tauri::async_runtime::spawn(async move {
                if me.verifier.replay(&me.client, out, Some(rom)).await.is_err() {
                    // the resume point may descend from the block that failed: start again from the checkpoint
                    me.verifier.set_resume(None);
                    let _ = me.verifier.check(&me.client, &cp, false).await;
                }
            });
        }
        #[cfg(mobile)]
        let _ = out;
        Ok(())
    }

    // ---- calls ----------------------------------------------------------------------------------------------------------

    /// An allowlisted JSON-RPC call with failover over every transport; resolves with the whole JSON-RPC answer
    /// (`{"result": ..}` or `{"error": ..}`).
    pub async fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        if !allowlist::rpc_allowed(method) {
            return Err(format!("RPC method {method} is not available"));
        }
        if allowlist::pinned_only(method) {
            // carries the view key: only to a node the caller names (Remote::rpc pins it), never to a failover pick
            return Err(format!("{method} is only sent to the node you chose"));
        }
        let m = match map_request(method, &params) {
            Some(m) => m,
            None => Mapped { req: Request::JsonRpc { method: method.to_string(), params: params.to_string() }, submit: false },
        };
        let (key, resp) = if m.submit {
            self.submit_any(&m.req).await?
        } else {
            self.client.read(&m.req, if m.req.is_bulk() { self.bulk_timeout } else { self.read_timeout }).await.map_err(|e| e.to_string())?
        };
        self.note(&key, method);
        shape(method, resp)
    }

    /// The chain height (blocks, genesis included) from any node's tip, HTTP or iroh.
    pub async fn height(&self) -> Result<u64, String> {
        match self.client.read(&Request::GetTip, self.read_timeout).await.map_err(|e| e.to_string())? {
            (key, Response::Tip(t)) => {
                self.note(&key, "getheight");
                Ok(t.height + 1)
            }
            (_, other) => Err(format!("unexpected answer: {other:?}")),
        }
    }

    /// A call to exactly one HTTP node (the one the user confirmed): no failover, no mapping.
    pub async fn rpc_pinned(&self, url: &str, method: &str, params: Value) -> Result<Value, String> {
        let Some((up, _)) = self.conn.http_entry(url) else { return Err("not an http(s) node".into()) };
        up.json_rpc(method, params, self.bulk_timeout).await.map_err(|e| e.to_string())
    }

    /// A submission goes to three nodes at once: Accepted by any is Accepted; else the first Rejected; else the failures.
    async fn submit_any(&self, req: &Request) -> Result<(String, Response), String> {
        let outs = self.client.submit(req, self.read_timeout).await.map_err(|e| e.to_string())?;
        let mut rejected = None;
        let mut errs = Vec::new();
        for (key, r) in outs {
            match r {
                Ok(Response::Accepted) => return Ok((key, Response::Accepted)),
                Ok(r @ Response::Rejected { .. }) => rejected = rejected.or(Some((key, r))),
                Ok(Response::Error(e)) => errs.push(format!("{key}: {e:?}")),
                Ok(other) => errs.push(format!("{key}: unexpected {other:?}")),
                Err(e) => errs.push(format!("{key}: {e}")),
            }
        }
        rejected.ok_or_else(|| format!("every node failed: {}", errs.join("; ")))
    }

    /// The wallet's daemon calls that are not JSON-RPC: `.bin` (epee) calls and `/send_raw_transaction`. Returns (content type, body).
    pub async fn wallet_post(&self, path: &str, body: Vec<u8>) -> Result<(&'static str, Vec<u8>), String> {
        let bin = match path {
            "/getblocks.bin" => Some(WalletBin::GetBlocks),
            "/get_outs.bin" => Some(WalletBin::GetOuts),
            "/gethashes.bin" => Some(WalletBin::GetHashes),
            "/get_transaction_pool_hashes.bin" => Some(WalletBin::GetPoolHashes),
            _ => None,
        };
        if let Some(call) = bin {
            let (key, resp) = self.client.read(&Request::Wallet { call, body }, self.bulk_timeout).await.map_err(|e| e.to_string())?;
            self.note(&key, path);
            return match resp {
                Response::Bin(b) => Ok(("application/octet-stream", b)),
                other => Err(format!("unexpected answer: {other:?}")),
            };
        }
        if path == "/send_raw_transaction" || path == "/sendrawtransaction" {
            let v: Value = serde_json::from_slice(&body).map_err(|e| format!("bad request: {e}"))?;
            if v.get("do_not_relay").and_then(|b| b.as_bool()) == Some(true) {
                return Err("do_not_relay is not supported through the proxy".into());
            }
            let blob = v.get("tx_as_hex").and_then(|h| h.as_str()).and_then(|h| hex::decode(h).ok()).ok_or("no tx_as_hex")?;
            let (key, resp) = self.submit_any(&Request::SendTx { blob }).await?;
            self.note(&key, path);
            let out = match resp {
                Response::Accepted => json!({"status": "OK"}),
                Response::Rejected { reason } => json!({"status": "Failed", "reason": reason}),
                other => return Err(format!("unexpected answer: {other:?}")),
            };
            return Ok(("application/json", out.to_string().into_bytes()));
        }
        if allowlist::path_allowed(path) {
            // every other wallet call (get_transactions, is_key_image_spent, get_o_indexes.bin ...): the passthrough request
            let req = Request::Path { path: path.to_string(), body };
            let (key, resp) = self.client.read(&req, self.bulk_timeout).await.map_err(|e| e.to_string())?;
            self.note(&key, path);
            return match resp {
                Response::Bin(b) => Ok((if path.ends_with(".bin") { "application/octet-stream" } else { "application/json" }, b)),
                other => Err(format!("unexpected answer: {other:?}")),
            };
        }
        Err(format!("{path} is not available"))
    }
}

/// Clients use discovery `none`: they never publish themselves (no pkarr / DHT record of this app's endpoint id and addresses
/// exists, so nobody can look the user up) and resolve nobody; they dial the nodes they know by endpoint id and relay URL (the
/// compiled-in bootstrap, learned `GetNodes` announcements, nodes the user added). Only serving nodes publish.
/// `SM64COIN_LC_DISCOVERY` = none | n0 | dht overrides (opt-in; tests and closed networks use none).
pub fn app_discovery() -> Discovery {
    match std::env::var("SM64COIN_LC_DISCOVERY").as_deref() {
        Ok("n0") => Discovery::N0,
        #[cfg(not(target_os = "android"))]
        Ok("dht") => Discovery::Dht,
        _ => Discovery::None,
    }
}

struct Mapped {
    req: Request,
    submit: bool,
}

fn only_keys(p: &Value, keys: &[&str]) -> bool {
    p.as_object().is_none_or(|o| o.keys().all(|k| keys.contains(&k.as_str())))
}

/// The protocol message of a JSON-RPC call, when it has one and `params` carry nothing the message cannot (the miner's
/// `sm64_*` extras, `get_block` by hash ...). None: the call is HTTP-only.
fn map_request(method: &str, p: &Value) -> Option<Mapped> {
    let u = |k: &str| p.get(k).and_then(|v| v.as_u64());
    let s = |k: &str| p.get(k).and_then(|v| v.as_str());
    let (req, submit) = match method {
        "get_block_headers_range" if only_keys(p, &["start_height", "end_height"]) => {
            let (a, b) = (u("start_height")?, u("end_height")?);
            (Request::GetHeaders { from: a, count: u32::try_from(b.checked_sub(a)? + 1).ok()? }, false)
        }
        "get_block" if only_keys(p, &["height"]) => (Request::GetBlocks { from: u("height")?, count: 1, max_bytes: 4 << 20 }, false),
        "get_sm64_cycle" if only_keys(p, &["hash"]) => (Request::GetCycle { hash: s("hash").unwrap_or("").to_string() }, false),
        "get_block_template" if only_keys(p, &["wallet_address", "reserve_size"]) => (Request::GetTemplate { address: s("wallet_address")?.to_string(), reserve: u("reserve_size").unwrap_or(0) as u32 }, false),
        "get_fee_estimate" => (Request::GetFee, false),
        "get_output_distribution" if only_keys(p, &["amounts", "from_height", "to_height", "cumulative"]) && p.get("cumulative").and_then(|c| c.as_bool()) != Some(true) => {
            let amounts: Vec<u64> = p.get("amounts").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_u64()).collect()).unwrap_or_default();
            (Request::GetOutDistribution { amounts, from_height: u("from_height").unwrap_or(0), to_height: u("to_height").unwrap_or(0) }, false)
        }
        "submit_block" => {
            let hex_blob = p.as_array().filter(|a| a.len() == 1).and_then(|a| a[0].as_str())?;
            (Request::SubmitBlock { blob: hex::decode(hex_blob).ok()? }, true)
        }
        "submit_sm64_proof" if only_keys(p, &["blob"]) => (Request::SubmitProof { blob: hex::decode(s("blob")?).ok()? }, true),
        _ => return None,
    };
    Some(Mapped { req, submit })
}

/// A protocol answer in the shape of the node's JSON-RPC answer.
fn shape(method: &str, resp: Response) -> Result<Value, String> {
    let ok = |v: Value| Ok(json!({"jsonrpc": "2.0", "id": 0, "result": v}));
    match resp {
        Response::JsonRpc(s) => serde_json::from_str(&s).map_err(|e| e.to_string()),
        Response::Json(s) => ok(serde_json::from_str(&s).map_err(|e| e.to_string())?),
        Response::Blocks(v) => match v.first() {
            Some(b) => ok(serde_json::from_str(b).map_err(|e| e.to_string())?),
            None => Ok(json!({"jsonrpc": "2.0", "id": 0, "error": {"code": -2, "message": "block not found"}})),
        },
        Response::Accepted if method == "submit_sm64_proof" => ok(json!({"status": "OK", "result": 1})),
        Response::Accepted => ok(json!({"status": "OK"})),
        Response::Rejected { reason } => Ok(json!({"jsonrpc": "2.0", "id": 0, "error": {"code": -7, "message": reason}})),
        Response::Error(ErrorKind::NotFound) => Ok(json!({"jsonrpc": "2.0", "id": 0, "error": {"code": -2, "message": "not found"}})),
        other => Err(format!("unexpected answer to {method}: {other:?}")),
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn genesis_matches_the_page() {
        let js = include_str!("../../../web/src/lib/nodechain.js");
        assert!(js.contains(&format!("'{MAINNET_GENESIS}'")), "update MAINNET_GENESIS (lcnet.rs) with nodechain.js");
    }

    #[test]
    fn the_view_key_call_is_not_routed_by_failover() {
        let net = Net::new(&[NodeAddr::Http { url: "https://a.example".into() }], None, None);
        let e = tauri::async_runtime::block_on(net.rpc("sm64_board_join", json!({}))).unwrap_err();
        assert!(e.contains("only sent to the node you chose"), "{e}");
    }

    #[test]
    fn mapping_is_exact() {
        assert!(map_request("get_sm64_cycle", &json!({})).is_some());
        assert!(map_request("get_block", &json!({"height": 5})).is_some());
        assert!(map_request("get_block", &json!({"height": 5, "sm64_played": true})).is_none(), "extras: HTTP only");
        assert!(map_request("get_block", &json!({"hash": "ab"})).is_none());
        assert!(map_request("get_block_template", &json!({"wallet_address": "A", "reserve_size": 0, "sm64_frames": 3})).is_none());
        assert!(map_request("get_info", &json!({})).is_none());
        let m = map_request("submit_block", &json!(["00ff"])).unwrap();
        assert!(m.submit && m.req == Request::SubmitBlock { blob: vec![0, 255] });
        assert!(map_request("get_block_headers_range", &json!({"start_height": 5, "end_height": 4})).is_none());
    }
}
