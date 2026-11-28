//! The app's light-client network: `sm64coin_lc::client::Client` over a scored `NodeList` (compiled-in bootstrap nodes, the node
//! the user confirmed, learned ones kept in `nodes.json`), over HTTP(S) JSON-RPC with failover between
//! them. `remote.rs` (the page's `remote_rpc`) and `walletproxy.rs` (the wallet's and the Android miner's daemon) both
//! go through here, so there is one list, one set of scores and one "which node is in use".
//!
//! Which calls fail over over which transport: every allowlisted call. A request the light-client protocol has a typed message
//! for (the wallet's binary calls, `get_sm64_cycle`, `get_fee_estimate`, `get_block_headers_range`, a plain `submit_block` ...)
//! is that message; every other allowlisted JSON-RPC call (`get_info`, `get_sm64_state`, a template with the miner's `sm64_*`
//! extras, `get_block` by hash ...) is `Request::JsonRpc` and every other wallet path (`/get_transactions`, `/is_key_image_spent`,
//! `/get_o_indexes.bin` ...) is `Request::Path`; all of them go to any node, and submissions go to three nodes at
//! once. A node too old for the passthrough requests answers `Unsupported` and the client moves on to the next.
//!
//! Verification (lcverify.rs): on startup and on each new tip the chain is checked from the compiled-in checkpoint across up to
//! five nodes; afterwards reads use only the nodes on the verified best branch, and nodes that lied are banned.
use serde_json::{Value, json};
use sm64coin_lc::allowlist;
use sm64coin_lc::client::{BULK_TIMEOUT, Client, Connector, Env, READ_TIMEOUT, SystemEnv, Transport, UpstreamTransport};
use sm64coin_lc::exec::Upstream;
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::nodes::{NodeAddr, NodeList, Source};
use sm64coin_lc::proto::{ErrorKind, Request, Response, TipInfo, WalletBin};
use crate::lcverify::{Verification, Verifier, checkpoint_for};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// v0.5 mainnet's genesis block hash (web/src/lib/nodechain.js `MAINNET_GENESIS`): the verifier's anchor.
pub const MAINNET_GENESIS: &str = "6b7164fc12bd735bfe29caf87615ec62ca318f78539afae11a13a244eb080482";

/// A node the last call went to.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Used {
    /// The node's key (`http:https://host`).
    pub node: String,
    /// `https` or `http`.
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
    pub nodes: Vec<NodeInfo>,
    /// What the verifier found (see lcverify.rs): `verified (n nodes agree)`, `conflict`, `unverified`.
    pub verify: Verification,
}

/// The chain tip, followed once for the whole app and fanned out: to the page (the `tip` event, lctip.rs), to the verifier
/// (a check per new tip, not a poll) and to the native miner ([`TipHub::feed`]: a new block ends its search). Two sources: the
/// light-client network (Server-Sent Events from a node that serves `/lc/tip`, else a get_info poll every 30 s: [`Net::tip_task`]) and,
/// on desktop, the app's own node (polled once a second: it is local; desktop.rs). The local node's tip wins while it keeps
/// arriving (it is the one this machine's miner builds on).
pub struct TipHub {
    tx: tokio::sync::watch::Sender<Option<TipInfo>>,
    local_at: Mutex<Option<std::time::Instant>>,
    /// No node answered the last attempts to learn the tip.
    stranded: AtomicBool,
    /// A push subscription is delivering (the network's tips arrive as they happen, not by a 30 s poll).
    pushing: AtomicBool,
    /// The tip source's clock minus ours, seconds (from a pushed record's server time; 0 for the local node or a poll).
    skew: std::sync::atomic::AtomicI64,
}

/// A local tip older than this no longer shadows the network's.
const LOCAL_FRESH: Duration = Duration::from_secs(10);

impl TipHub {
    pub fn new() -> Arc<TipHub> {
        Arc::new(TipHub { tx: tokio::sync::watch::channel(None).0, local_at: Mutex::new(None), stranded: AtomicBool::new(false), pushing: AtomicBool::new(false), skew: Default::default() })
    }
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Option<TipInfo>> {
        self.tx.subscribe()
    }
    pub fn current(&self) -> Option<TipInfo> {
        self.tx.borrow().clone()
    }
    /// True while no source could tell the tip (every node failed): the verifier then looks again soon.
    pub fn stranded(&self) -> bool {
        self.stranded.load(Ordering::Relaxed)
    }
    fn set(&self, t: TipInfo) {
        // a change is a new block (or a new difficulty): not every poll
        self.tx.send_if_modified(|cur| {
            if cur.as_ref().is_some_and(|c| c.hash == t.hash && c.difficulty == t.difficulty) {
                return false;
            }
            *cur = Some(t);
            true
        });
    }
    /// The app's own node's tip (desktop).
    #[cfg_attr(not(desktop), allow(dead_code))]
    pub fn offer_local(&self, t: TipInfo) {
        *self.local_at.lock().unwrap() = Some(std::time::Instant::now());
        self.stranded.store(false, Ordering::Relaxed);
        self.skew.store(0, Ordering::Relaxed); // the same machine's clock
        self.set(t);
    }
    fn local_fresh(&self) -> bool {
        self.local_at.lock().unwrap().is_some_and(|at| at.elapsed() < LOCAL_FRESH)
    }
    /// A tip from the light-client network; ignored while the local node's keeps arriving. `server_time` (when nonzero) is the
    /// sender's clock at sending: the page's clock is corrected by it (the node judges a held run's send time by its own clock).
    pub fn offer_net(&self, t: TipInfo) {
        self.stranded.store(false, Ordering::Relaxed);
        if self.local_fresh() {
            return;
        }
        if t.server_time > 0 {
            self.skew.store(t.server_time as i64 - now_secs() as i64, Ordering::Relaxed);
        }
        self.set(t);
    }
    /// A pushed tip from the network (see [`TipHub::offer_net`]); marks the push as delivering.
    pub fn offer_pushed(&self, t: TipInfo) {
        self.pushing.store(true, Ordering::Relaxed);
        self.offer_net(t);
    }
    pub fn set_pushing(&self, on: bool) {
        self.pushing.store(on, Ordering::Relaxed);
    }
    /// The tip with `server_time` the source's clock NOW (ours corrected by the last pushed reading): what the page is told, so a
    /// record read long after the tip came (`tip_get`) still gives the right clock.
    pub fn stamped(&self, t: TipInfo) -> TipInfo {
        let skew = if self.local_fresh() { 0 } else { self.skew.load(Ordering::Relaxed) };
        TipInfo { server_time: (now_secs() as i64 + skew).max(0) as u64, ..t }
    }
    pub fn set_stranded(&self, on: bool) {
        self.stranded.store(on, Ordering::Relaxed);
    }
    /// The tip's block id (hex), no request.
    #[cfg_attr(desktop, allow(dead_code))]
    pub fn tip_hash(&self) -> Option<String> {
        self.tx.borrow().as_ref().map(|t| hex::encode(t.hash))
    }
    /// What the native miner's feed reads: the tip while it is live (pushed, or the local node polled each second), else None so
    /// the campaign polls its daemon itself (a 30 s network poll would tell it of a new block up to 30 s late).
    #[cfg_attr(desktop, allow(dead_code))]
    pub fn live_tip_hash(&self) -> Option<String> {
        if self.pushing.load(Ordering::Relaxed) || self.local_fresh() { self.tip_hash() } else { None }
    }
}

pub fn transport_of(addr: &NodeAddr) -> &'static str {
    match addr {
        NodeAddr::Http { url, .. } if url.starts_with("https://") => "https",
        NodeAddr::Http { .. } => "http",
    }
}

/// Builds the transport of a node (HTTP(S)).
struct AppConnector {
    http: Mutex<HashMap<String, (HttpUpstream, Arc<dyn Transport>)>>,
}

impl AppConnector {
    fn http_entry(&self, url: &str, tip: Option<&str>) -> Option<(HttpUpstream, Arc<dyn Transport>)> {
        let mut m = self.http.lock().unwrap();
        let key = format!("{url}\n{}", tip.unwrap_or(""));
        if let Some(e) = m.get(&key) {
            return Some(e.clone());
        }
        let up = HttpUpstream::new(url).ok()?.with_tip_url(tip.map(str::to_string));
        let e: (HttpUpstream, Arc<dyn Transport>) = (up.clone(), Arc::new(UpstreamTransport(up)));
        m.insert(key, e.clone());
        Some(e)
    }
}

impl Connector for AppConnector {
    fn transport(&self, addr: &NodeAddr) -> Option<Arc<dyn Transport>> {
        let NodeAddr::Http { url, tip } = addr;
        self.http_entry(url, tip.as_deref()).map(|e| e.1)
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
    proxy: Mutex<Option<String>>,
    verifier: Arc<Verifier>,
    hub: Arc<TipHub>,
    /// Desktop: where to get the ROM for the background replay (None: header layer only).
    rom_finder: Mutex<Option<RomFinder>>,
    /// The chain's genesis block hash (the verifier's anchor).
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
            list.add(NodeAddr::http(s.to_string()), Source::Manual, now);
        }
        let conn = Arc::new(AppConnector { http: Mutex::new(HashMap::new()) });
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
            proxy: Mutex::new(None),
            verifier,
            hub: TipHub::new(),
            rom_finder: Mutex::new(None),
            network: network_genesis(),
            read_timeout: READ_TIMEOUT,
            bulk_timeout: BULK_TIMEOUT,
        })
    }

    /// The app's tip hub (see [`TipHub`]).
    pub fn tip_hub(&self) -> Arc<TipHub> {
        self.hub.clone()
    }

    /// One look at the chain tip through the network (any node): `get_info`, and the tip block's header when the
    /// tip is not the one the hub holds. The fallback of the push (nodes without `/lc/tip`).
    pub async fn poll_tip(&self) -> Result<TipInfo, String> {
        use sm64coin_lc::exec::{header_timestamp, tip_info_from};
        let result = |v: Value| -> Result<Value, String> {
            if let Some(e) = v.get("error") {
                return Err(e.get("message").and_then(|m| m.as_str()).unwrap_or("error").to_string());
            }
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        };
        let info = result(self.rpc("get_info", json!({})).await?)?;
        let tip = sm64coin_lc::exec::tip_from_info(&info).ok_or("unreadable get_info")?;
        let known = self.hub.current().filter(|k| k.hash == tip.hash);
        let timestamp = match known {
            Some(k) => k.timestamp,
            None => {
                let h = result(self.rpc("get_last_block_header", json!({})).await?)?;
                header_timestamp(&h, tip.hash).ok_or("tip moved")?
            }
        };
        tip_info_from(&info, timestamp, now_secs()).ok_or_else(|| "unreadable get_info".to_string())
    }

    /// Follows the tip for the hub: by push when a node serves `GET /lc/tip` (Server-Sent Events), else (or when none does) a
    /// poll every 30 s, trying the push again every ten minutes. Runs for the life of the app.
    pub async fn tip_task(self: Arc<Self>) {
        use sm64coin_lc::client::WatchEnd;
        loop {
            {
                let hub = self.hub.clone();
                let end = self.client.watch_tip(|_, t| hub.offer_pushed(t)).await;
                self.hub.set_pushing(false);
                match end {
                    WatchEnd::NoPush => {}
                    WatchEnd::NoNodes => self.hub.set_stranded(true),
                }
            }
            let until = std::time::Instant::now() + Duration::from_secs(600);
            loop {
                match self.poll_tip().await {
                    Ok(t) => self.hub.offer_net(t),
                    Err(_) => self.hub.set_stranded(true),
                }
                tokio::time::sleep(Duration::from_secs(30)).await;
                if std::time::Instant::now() > until {
                    break;
                }
            }
        }
    }

    /// The user's confirmed node changed (or was forgotten).
    pub fn set_saved(&self, old: Option<&str>, new: Option<&str>) {
        let now = now_secs();
        self.client.with_list(|l| {
            if let Some(o) = old {
                l.remove(&NodeAddr::http(o.to_string()).key());
            }
            if let Some(n) = new {
                l.add(NodeAddr::http(n.to_string()), Source::Manual, now);
            }
        });
    }

    pub fn set_proxy(&self, url: &str) {
        *self.proxy.lock().unwrap() = Some(url.to_string());
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

    /// Asks up to two nodes for the public nodes they know (`/get_public_nodes`, Monero's own list of peers that serve a public RPC)
    /// and adds the ones with a public IP address; the verifier and the chain probe find out which serve the right chain.
    /// Returns how many were new.
    pub async fn discover(&self) -> usize {
        let body = br#"{"white":true,"gray":false}"#.to_vec();
        let mut learned = 0;
        for _ in 0..2 {
            match self.client.read(&Request::Path { path: "/get_public_nodes".into(), body: body.clone() }, self.read_timeout).await {
                Ok((_, Response::Bin(b))) => learned += self.client.learn(sm64coin_lc::nodes::parse_public_nodes(&b, sm64coin_lc::client::LEARN_PER_ANSWER)),
                _ => {}
            }
        }
        learned
    }

    /// Background work of the app: follows the tip (push, else poll), verifies the chain (on startup and on every new tip),
    /// every minute persists the list and every ten minutes looks for more public nodes.
    pub fn spawn_background(self: &Arc<Self>) {
        tauri::async_runtime::spawn(self.clone().tip_task());
        let me = self.clone();
        tauri::async_runtime::spawn(async move { me.maintain().await });
    }

    async fn maintain(self: &Arc<Self>) {
        let mut n = 0u64;
        let mut last_verify: Option<std::time::Instant> = None;
        let mut rx = self.hub.subscribe();
        loop {
            // The chain is checked when the hub announces a new tip (push), and the timed conditions (an unverified result,
            // ten minutes) every 20 s from the clock: no request is made here to learn the tip.
            let pushed = matches!(tokio::time::timeout(Duration::from_secs(if n == 0 { 2 } else { 5 }), rx.changed()).await, Ok(Ok(())));
            if !pushed {
                if n % 120 == 0 {
                    self.discover().await;
                }
                if n % 12 == 0 {
                    self.persist();
                }
            }
            if pushed || n % 4 == 0 {
                // verify: the first time, when the tip moved, after an unverified result (every minute), and at least every ten minutes
                let v = self.verifier.get();
                let tip = self.hub.current().map(|t| sm64coin_lc::proto::Tip { height: t.height, hash: t.hash, cumulative_difficulty: t.cumulative_difficulty });
                let moved = match (&tip, v.height) {
                    (Some(t), Some(h)) => t.height > h || (t.height == h && Some(hex::encode(t.hash)) != v.tip),
                    (Some(_), None) => true,
                    (None, _) => false,
                };
                let since = last_verify.map(|t| t.elapsed());
                // no verified node answers any more: check again soon (the check asks any node, and either finds the new verified
                // set or lifts the restriction), so the restriction never leaves the app without a node for long
                let stranded = (tip.is_none() || self.hub.stranded()) && v.state != "unverified";
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
            if !pushed {
                n += 1;
            }
        }
    }

    fn note(&self, key: &str, method: &str) {
        let transport = if key.starts_with("http:https://") { "https" } else { "http" };
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
        NetStatus { proxy: self.proxy.lock().unwrap().clone(), used: self.used.lock().unwrap().clone(), nodes, verify: self.verifier.get() }
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
            // It is CPU-heavy (the emulator, up to two minutes): wait until the app has painted and the first startup work is
            // done, so opening the app is not competing with a replay. `SM64COIN_NO_REPLAY=1` turns it off entirely.
            let me = self.clone();
            tauri::async_runtime::spawn(async move {
                if std::env::var_os("SM64COIN_NO_REPLAY").is_some() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(45)).await;
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

    /// The chain height (blocks, genesis included) from any node's tip.
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
        let Some((up, _)) = self.conn.http_entry(url, None) else { return Err("not an http(s) node".into()) };
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
        let net = Net::new(&[NodeAddr::http("https://a.example".into())], None, None);
        let e = tauri::async_runtime::block_on(net.rpc("sm64_board_join", json!({}))).unwrap_err();
        assert!(e.contains("only sent to the node you chose"), "{e}");
    }

    fn tip(h: u8, ts: u64) -> TipInfo {
        TipInfo { height: h as u64, hash: [h; 32], timestamp: ts, cumulative_difficulty: 1, difficulty: 1000, server_time: ts }
    }

    #[test]
    fn the_tip_hub_notifies_only_changes_and_the_local_node_shadows_the_network() {
        let hub = TipHub::new();
        let mut rx = hub.subscribe();
        assert!(hub.current().is_none() && hub.tip_hash().is_none());
        hub.offer_net(tip(1, 10));
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();
        hub.offer_net(tip(1, 10)); // the same block again (a poll, a reconnect): no change
        assert!(!rx.has_changed().unwrap());
        hub.offer_net(tip(2, 20));
        assert!(rx.has_changed().unwrap());
        assert_eq!(hub.tip_hash().unwrap(), hex::encode([2u8; 32]));
        rx.borrow_and_update();
        // the app's own node speaks: its tip is used, the network's is ignored while it keeps arriving
        hub.offer_local(tip(3, 30));
        hub.offer_net(tip(4, 40));
        assert_eq!(hub.current().unwrap().height, 3);
        hub.offer_local(tip(5, 50));
        assert_eq!(hub.current().unwrap().height, 5);
        // stranded: set by a failed attempt, cleared by any tip
        hub.set_stranded(true);
        assert!(hub.stranded());
        hub.offer_local(tip(5, 50));
        assert!(!hub.stranded());
    }

    #[test]
    fn the_tip_hub_tells_the_page_the_senders_clock_now_and_the_miner_only_a_live_tip() {
        let hub = TipHub::new();
        // a pushed tip from a server 40 s behind this device, read by the page much later: still the server's clock NOW
        let now = now_secs();
        hub.offer_pushed(TipInfo { server_time: now - 40, ..tip(1, 10) });
        let st = hub.stamped(hub.current().unwrap()).server_time as i64;
        assert!((st - (now as i64 - 40)).abs() <= 1, "{st} vs {now}");
        assert_eq!(hub.live_tip_hash(), hub.tip_hash(), "pushing: the miner reads the hub");
        // the push ended: the hub's tip may be up to a poll old, so the miner polls its daemon itself
        hub.set_pushing(false);
        hub.offer_net(tip(2, 20));
        assert!(hub.tip_hash().is_some() && hub.live_tip_hash().is_none());
        // the local node (desktop): live, and its clock is ours
        hub.offer_local(tip(3, 30));
        assert!(hub.live_tip_hash().is_some());
        assert!((hub.stamped(hub.current().unwrap()).server_time as i64 - now_secs() as i64).abs() <= 1);
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
