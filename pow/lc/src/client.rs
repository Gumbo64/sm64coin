//! The client: one [`Transport`] trait (HTTP(S) JSON-RPC) and [`Client`], which
//! picks nodes from a [`NodeList`], fails over, fans submissions out to several nodes and feeds
//! latency, errors and tip heights back into the scores.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::exec::{self, MaybeSend, MaybeSync, Upstream};
use crate::nodes::{NodeAddr, NodeList, NodeRecord};
use crate::proto::*;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("cannot connect: {0}")]
    Connect(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The peer (or this transport) does not do that (a node without the tip stream).
    #[error("not supported by this node")]
    Unsupported,
}

/// A tip subscription (Server-Sent Events, `GET /lc/tip`) as the client reads it.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait TipStream: MaybeSend {
    /// The next pushed tip (keepalives are swallowed); `Ok(None)`: the server ended the stream. Dropping the stream ends it.
    async fn next(&mut self) -> Result<Option<TipInfo>, TransportError>;
}

/// One way of asking a node a [`Request`].
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait Transport: MaybeSend + MaybeSync {
    async fn request(&self, req: &Request, timeout: Duration) -> Result<Response, TransportError>;
    /// Opens a tip subscription. The default cannot push: `Unsupported`, and the caller polls instead.
    async fn open_tip_stream(&self) -> Result<Box<dyn TipStream>, TransportError> {
        Err(TransportError::Unsupported)
    }
}

/// The JSON-RPC of a node over HTTP(S) as a [`Transport`] (the [`Upstream`] decides how the bytes travel).
pub struct UpstreamTransport<U: Upstream>(pub U);

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl<U: Upstream> Transport for UpstreamTransport<U> {
    async fn request(&self, req: &Request, timeout: Duration) -> Result<Response, TransportError> {
        Ok(exec::run(&self.0, req, timeout).await)
    }
    async fn open_tip_stream(&self) -> Result<Box<dyn TipStream>, TransportError> {
        self.0.tip_stream().await.unwrap_or(Err(TransportError::Unsupported))
    }
}

/// Builds the transport for a node address (HTTP(S)). The browser supplies its own.
pub trait Connector: MaybeSend + MaybeSync {
    fn transport(&self, addr: &NodeAddr) -> Option<Arc<dyn Transport>>;
}

/// Time and randomness for the pure logic (`Instant` does not work in the browser).
pub trait Env: MaybeSend + MaybeSync {
    fn now_ms(&self) -> u64;
    fn rand(&self) -> u64;
}

#[cfg(not(target_arch = "wasm32"))]
pub struct SystemEnv;
#[cfg(not(target_arch = "wasm32"))]
impl Env for SystemEnv {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }
    fn rand(&self) -> u64 {
        use std::hash::{BuildHasher, Hasher};
        std::collections::hash_map::RandomState::new().build_hasher().finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("no usable node")]
    NoNodes,
    #[error("every node failed: {0}")]
    AllFailed(String),
    /// A node answered, and the answer is a definite "no" (not a node fault): no failover.
    #[error("request refused: {0:?}")]
    Refused(ErrorKind),
}

/// How many nodes a failing read tries before giving up.
pub const MAX_ATTEMPTS: usize = 4;
/// Fan-out of submissions (`submit_block`, proofs, `sendrawtransaction`).
pub const SUBMIT_FANOUT: usize = 3;
/// Default timeouts (doc: 10 s; 60 s for bulk block reads).
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);
pub const BULK_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Client {
    list: Mutex<NodeList>,
    /// When set, reads and submissions use only these nodes (the verified best branch's; see `verify`). Verification itself
    /// ([`Client::pick_keys`], [`Client::ask_node`]) is not restricted.
    allowed: Mutex<Option<std::collections::HashSet<String>>>,
    connector: Arc<dyn Connector>,
    env: Arc<dyn Env>,
}

impl Client {
    pub fn new(list: NodeList, connector: Arc<dyn Connector>, env: Arc<dyn Env>) -> Self {
        Self { list: Mutex::new(list), allowed: Mutex::new(None), connector, env }
    }
    fn now(&self) -> u64 {
        self.env.now_ms() / 1000
    }
    /// A snapshot of the node list (to persist or show).
    pub fn list_json(&self) -> String {
        self.list.lock().unwrap().to_json()
    }
    pub fn with_list<R>(&self, f: impl FnOnce(&mut NodeList) -> R) -> R {
        f(&mut self.list.lock().unwrap())
    }
    /// A random number from the environment (spot-check heights must not be predictable by the node checked).
    pub fn rand(&self) -> u64 {
        self.env.rand()
    }
    pub fn ban_liar(&self, key: &str) {
        let now = self.now();
        self.list.lock().unwrap().ban_liar(key, now);
    }
    /// Up to `n` distinct node keys, best first (weighted). For callers that ask nodes one by one
    /// ([`Client::ask_node`]), like the light-client verifier's cross-check.
    pub fn pick_keys(&self, n: usize) -> Vec<String> {
        self.pick_unrestricted(n).into_iter().map(|(k, _)| k).collect()
    }

    /// Asks one specific node (no failover): the answer is attributable. Latency and errors still feed the score.
    pub async fn ask_node(&self, key: &str, req: &Request, timeout: Duration) -> Result<Response, String> {
        let addr = self.list.lock().unwrap().get(key).map(|r| r.addr.clone()).ok_or_else(|| format!("unknown node {key}"))?;
        self.ask(key, &addr, req, timeout).await
    }

    /// Restricts reads and submissions to these node keys (`None`: any node). An empty set is treated as `None`.
    pub fn set_allowed(&self, keys: Option<Vec<String>>) {
        *self.allowed.lock().unwrap() = keys.filter(|k| !k.is_empty()).map(|k| k.into_iter().collect());
    }
    pub fn allowed(&self) -> Option<Vec<String>> {
        self.allowed.lock().unwrap().as_ref().map(|s| s.iter().cloned().collect())
    }

    fn pick_unrestricted(&self, n: usize) -> Vec<(String, NodeAddr)> {
        let now = self.now();
        let r = self.env.rand();
        self.list.lock().unwrap().pick_many(n, now, r).into_iter().map(|r: &NodeRecord| (r.key(), r.addr.clone())).collect()
    }

    fn pick(&self, n: usize) -> Vec<(String, NodeAddr)> {
        let allowed = self.allowed.lock().unwrap().clone();
        let Some(allowed) = allowed else {
            let v = self.pick_unrestricted(n);
            if !v.is_empty() {
                return v;
            }
            // Last resort, when every node is banned or behind (a verifier that rejects every node, e.g. after a rule
            // change it does not know): the best-scored records anyway, so the app is never left with no node at all. The
            // caller's verification state says the reads are unverified.
            let list = self.list.lock().unwrap();
            let mut all: Vec<&NodeRecord> = list.records().iter().collect();
            all.sort_by(|a, b| b.ok_rate.total_cmp(&a.ok_rate));
            return all.into_iter().take(n).map(|r| (r.key(), r.addr.clone())).collect();
        };
        let all = self.list.lock().unwrap().len();
        self.pick_unrestricted(all).into_iter().filter(|(k, _)| allowed.contains(k)).take(n).collect()
    }

    async fn ask(&self, key: &str, addr: &NodeAddr, req: &Request, timeout: Duration) -> Result<Response, String> {
        let Some(t) = self.connector.transport(addr) else {
            return Err("no transport for this node".into());
        };
        let t0 = self.env.now_ms();
        let res = t.request(req, timeout).await;
        let ms = self.env.now_ms().saturating_sub(t0) as f64;
        let now = self.now();
        let mut list = self.list.lock().unwrap();
        match res {
            Ok(Response::Error(ErrorKind::Upstream(_))) => {
                list.record_err(key, now);
                Err("node failing".into())
            }
            Ok(resp) => {
                let height = if let Response::Tip(t) = &resp { Some(t.height) } else { None };
                list.record_ok(key, ms, height, now);
                Ok(resp)
            }
            Err(e) => {
                list.record_err(key, now);
                Err(e.to_string())
            }
        }
    }

    /// An idempotent read: tries nodes best-first (weighted) until one answers. A definite refusal
    /// (`Malformed`, `NotFound`, `Unsupported`) is returned, not retried elsewhere.
    pub async fn read(&self, req: &Request, timeout: Duration) -> Result<(String, Response), ClientError> {
        let picks = self.pick(MAX_ATTEMPTS);
        if picks.is_empty() {
            return Err(ClientError::NoNodes);
        }
        let mut errs = Vec::new();
        for (key, addr) in picks {
            match self.ask(&key, &addr, req, timeout).await {
                // an older node does not know the passthrough requests (and a node may serve a narrower allowlist): the
                // next node may
                Ok(Response::Error(ErrorKind::Malformed | ErrorKind::Unsupported)) if req.is_passthrough() => errs.push(format!("{key}: not supported")),
                Ok(Response::Error(e)) => return Err(ClientError::Refused(e)),
                Ok(r) => return Ok((key, r)),
                Err(e) => errs.push(format!("{key}: {e}")),
            }
        }
        Err(ClientError::AllFailed(errs.join("; ")))
    }

    /// A submission goes to [`SUBMIT_FANOUT`] nodes at once; returns each node's outcome.
    pub async fn submit(&self, req: &Request, timeout: Duration) -> Result<Vec<(String, Result<Response, String>)>, ClientError> {
        let picks = self.pick(SUBMIT_FANOUT);
        if picks.is_empty() {
            return Err(ClientError::NoNodes);
        }
        let futs = picks.iter().map(|(k, a)| async move { (k.clone(), self.ask(k, a, req, timeout).await) });
        Ok(futures_util::future::join_all(futs).await)
    }

    /// Asks every candidate (up to `max`) for its tip: refreshes heights so lagging nodes are skipped.
    pub async fn refresh_tips(&self, max: usize, timeout: Duration) {
        let picks = self.pick(max);
        let futs = picks.iter().map(|(k, a)| async move { let _ = self.ask(k, a, &Request::GetTip, timeout).await; });
        futures_util::future::join_all(futs).await;
    }
}

#[cfg(feature = "http")]
async fn nap(d: Duration) {
    tokio::time::sleep(d).await
}

/// How a tip watch ended.
#[derive(Debug)]
pub enum WatchEnd {
    /// No node in the list serves the tip stream: poll instead.
    NoPush,
    /// No usable node at all, or [`WATCH_GIVE_UP_ROUNDS`] rounds in a row in which no node could be subscribed (all failing):
    /// the caller polls meanwhile and tries again later, so a dead push never leaves it without a tip.
    NoNodes,
}

/// Rounds in a row without any subscription (every node erred) after which [`Client::watch_tip`] gives up with `NoNodes`.
pub const WATCH_GIVE_UP_ROUNDS: u32 = 4;

#[cfg(feature = "http")]
impl Client {
    /// Follows the chain tip by push: subscribes at the best node, calls `on_tip(node key, tip)` for the current tip and each
    /// change, and on a broken stream reconnects (another node, with backoff up to 30 s). Runs until the future is dropped; it
    /// returns when no node can push (`WatchEnd::NoPush`: every node answered `Unsupported`) or none could be reached for
    /// [`WATCH_GIVE_UP_ROUNDS`] rounds (`WatchEnd::NoNodes`), so the caller can poll.
    pub async fn watch_tip<F: FnMut(&str, TipInfo) + MaybeSend>(&self, mut on_tip: F) -> WatchEnd {
        let mut unsupported = std::collections::HashSet::<String>::new();
        let mut backoff = Duration::from_secs(1);
        let mut failed_rounds = 0u32;
        loop {
            let picks: Vec<_> = self.pick(MAX_ATTEMPTS + unsupported.len()).into_iter().filter(|(k, _)| !unsupported.contains(k)).take(MAX_ATTEMPTS).collect();
            if picks.is_empty() {
                return if unsupported.is_empty() { WatchEnd::NoNodes } else { WatchEnd::NoPush };
            }
            let (mut subscribed, mut learned) = (false, false); // learned: a node turned out not to push (not a failure)
            for (key, addr) in picks {
                let Some(t) = self.connector.transport(&addr) else {
                    unsupported.insert(key);
                    learned = true;
                    continue;
                };
                let mut st = match t.open_tip_stream().await {
                    Ok(st) => st,
                    Err(TransportError::Unsupported) => {
                        unsupported.insert(key);
                        learned = true;
                        continue;
                    }
                    Err(_) => {
                        self.list.lock().unwrap().record_err(&key, self.now());
                        continue;
                    }
                };
                let mut got = false;
                loop {
                    match st.next().await {
                        Ok(Some(tip)) => {
                            got = true;
                            let now = self.now();
                            self.list.lock().unwrap().record_ok(&key, 0.0, Some(tip.height), now);
                            on_tip(&key, tip);
                        }
                        Ok(None) | Err(TransportError::Unsupported) => break,
                        Err(_) => {
                            self.list.lock().unwrap().record_err(&key, self.now());
                            break;
                        }
                    }
                }
                if got {
                    backoff = Duration::from_secs(1);
                }
                subscribed = true;
                break;
            }
            if subscribed {
                failed_rounds = 0;
            } else if !learned {
                failed_rounds += 1;
                if failed_rounds >= WATCH_GIVE_UP_ROUNDS {
                    return WatchEnd::NoNodes;
                }
            }
            nap(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }
}
