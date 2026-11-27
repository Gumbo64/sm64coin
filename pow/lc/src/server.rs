//! The light-client service: an iroh [`ProtocolHandler`] for [`ALPN`] that answers from the local
//! node's RPC (through the allowlist in [`crate::exec`]), with per-peer and per-IP budgets, a
//! concurrency cap, `Busy` instead of queueing, and signed node announcements for `GetNodes`.
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointId, SecretKey, TransportAddr, endpoint::Connection};

use crate::endpoint::sign_announcement;
use crate::exec::{self, Upstream};
use crate::proto::*;
use crate::tipfeed::{TipFeed, TipFeedConfig};

/// A token bucket.
#[derive(Clone, Copy, Debug)]
pub struct Bucket {
    pub tokens: f64,
    pub last: Instant,
}

impl Bucket {
    pub fn new(capacity: f64, now: Instant) -> Self {
        Self { tokens: capacity, last: now }
    }
    /// Takes `cost` tokens; `Err(retry_ms)` if the bucket is short.
    pub fn take(&mut self, cost: f64, capacity: f64, refill_per_sec: f64, now: Instant) -> Result<(), u32> {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + dt * refill_per_sec).min(capacity);
        self.last = now;
        if self.tokens >= cost {
            self.tokens -= cost;
            Ok(())
        } else {
            Err((((cost - self.tokens) / refill_per_sec) * 1000.0).ceil().clamp(1.0, 60_000.0) as u32)
        }
    }
    /// Charges `cost` after the fact (an answer turned out large): the bucket may go into debt, down to `-capacity`, and
    /// the next requests wait until it refills.
    pub fn debit(&mut self, cost: f64, capacity: f64) {
        self.tokens = (self.tokens - cost).max(-capacity);
    }
}

/// Extra cost of an answer of `bytes` (one token per 256 KiB above the first): a passthrough call is priced by what it
/// returned, not only by its name (`/get_outs.bin`, `getblocks.bin`, `get_transactions` can be large).
pub fn answer_cost(bytes: usize) -> f64 {
    (bytes.saturating_sub(256 << 10) as f64) / (256 << 10) as f64
}

#[derive(Clone, Debug)]
pub struct Limits {
    /// Bucket capacity (burst) in cost units, per endpoint id and per IP.
    pub burst: f64,
    /// Refill, cost units per second.
    pub refill_per_sec: f64,
    pub max_connections: usize,
    pub max_upstream_concurrent: usize,
    /// A request frame must arrive within this long.
    pub request_timeout: Duration,
    pub upstream_timeout: Duration,
    pub bulk_timeout: Duration,
    /// Malformed requests tolerated on one connection before it is closed.
    pub max_malformed: u32,
    /// Connections one IP may hold at once (relayed connections, which show no IP, are only under `max_connections`).
    pub max_conns_per_ip: usize,
    /// Requests that arrive over a relay show no IP: they share one budget of this many times an IP's.
    pub relayed_budget_mult: f64,
    /// How often the one shared watcher asks the local node for its tip while someone is subscribed (the node is local, so
    /// 1 s is cheap; subscribers cost the node nothing).
    pub tip_poll: Duration,
    /// ... and while nobody is (the tip in `Hello` and the announcement).
    pub tip_idle_poll: Duration,
    /// Silence on a tip subscription after which a `Keepalive` is sent.
    pub keepalive: Duration,
    pub max_subs_per_endpoint: usize,
    pub max_subs_total: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            burst: 60.0,
            refill_per_sec: 15.0,
            max_connections: 512,
            max_upstream_concurrent: 16,
            request_timeout: Duration::from_secs(10),
            upstream_timeout: Duration::from_secs(15),
            bulk_timeout: Duration::from_secs(60),
            max_malformed: 3,
            max_conns_per_ip: 16,
            relayed_budget_mult: 20.0,
            tip_poll: Duration::from_secs(1),
            tip_idle_poll: Duration::from_secs(5),
            keepalive: Duration::from_secs(KEEPALIVE_SECS),
            max_subs_per_endpoint: MAX_SUBS_PER_ENDPOINT,
            max_subs_total: MAX_SUBS_TOTAL,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Genesis block hash of the chain we serve.
    pub network: [u8; 32],
    pub limits: Limits,
    pub flags: u32,
    pub version: u32,
    pub https_rpc: Option<String>,
}

/// Endpoints that sent a valid `Hello` for our network recently: the relay admits only these.
#[derive(Clone, Debug, Default)]
pub struct HelloLog(Arc<Mutex<HashMap<EndpointId, Instant>>>);

impl HelloLog {
    pub fn record(&self, id: EndpointId) {
        let mut m = self.0.lock().unwrap();
        if m.len() > 50_000 {
            m.retain(|_, t| t.elapsed() < Duration::from_secs(3600));
        }
        m.insert(id, Instant::now());
    }
    pub fn seen_within(&self, id: &EndpointId, d: Duration) -> bool {
        self.0.lock().unwrap().get(id).is_some_and(|t| t.elapsed() < d)
    }
}

#[derive(Debug, Default)]
pub struct Stats {
    pub requests: AtomicU64,
    pub busy: AtomicU64,
    pub malformed: AtomicU64,
    pub connections: AtomicU64,
    /// Tip subscriptions opened / tip frames pushed (one per subscriber per change) / refused over a limit.
    pub subscriptions: AtomicU64,
    pub tips_pushed: AtomicU64,
    pub subs_refused: AtomicU64,
}

struct Inner {
    cfg: ServerConfig,
    upstream: Arc<dyn Upstream>,
    secret: SecretKey,
    endpoint: RwLock<Option<Endpoint>>,
    peer_buckets: Mutex<HashMap<EndpointId, Bucket>>,
    ip_buckets: Mutex<HashMap<IpAddr, Bucket>>,
    /// The shared budget of requests that show no IP (relayed).
    relayed_bucket: Mutex<Option<Bucket>>,
    ip_conns: Mutex<HashMap<IpAddr, usize>>,
    tip: RwLock<(u64, [u8; 32])>,
    /// The tip every subscriber watches; one watcher task feeds it ([`LcService::serve`]).
    feed: Arc<TipFeed>,
    relay_advertised: RwLock<bool>,
    known: Mutex<Vec<NodeAnnouncement>>,
    hello_log: HelloLog,
    conns: AtomicUsize,
    upstream_busy: AtomicUsize,
    stats: Stats,
}

/// The service. Cheap to clone; register it with [`LcService::serve`] or a [`Router`] under [`ALPN`].
#[derive(Clone)]
pub struct LcService(Arc<Inner>);

impl std::fmt::Debug for LcService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LcService").finish_non_exhaustive()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl LcService {
    pub fn new(cfg: ServerConfig, upstream: Arc<dyn Upstream>, secret: SecretKey, hello_log: HelloLog) -> Self {
        let feed = TipFeed::new(upstream.clone(), TipFeedConfig { poll: cfg.limits.tip_poll, idle_poll: cfg.limits.tip_idle_poll, keepalive: cfg.limits.keepalive, max_total: cfg.limits.max_subs_total });
        Self(Arc::new(Inner {
            cfg,
            upstream,
            secret,
            endpoint: RwLock::new(None),
            peer_buckets: Default::default(),
            ip_buckets: Default::default(),
            relayed_bucket: Default::default(),
            ip_conns: Default::default(),
            tip: RwLock::new((0, [0; 32])),
            feed,
            relay_advertised: RwLock::new(false),
            known: Default::default(),
            hello_log,
            conns: AtomicUsize::new(0),
            upstream_busy: AtomicUsize::new(0),
            stats: Stats::default(),
        }))
    }

    /// Spawns the router on `endpoint` and the tip refresher; shut the returned router down to stop.
    pub fn serve(&self, endpoint: Endpoint) -> Router {
        *self.0.endpoint.write().unwrap() = Some(endpoint.clone());
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                if me.0.endpoint.read().unwrap().as_ref().is_some_and(|e| e.is_closed()) {
                    break;
                }
                me.refresh_tip().await;
                // one local poll for all subscribers; slow while nobody listens
                tokio::time::sleep(if me.0.feed.subscribers() > 0 { me.0.feed.cfg.poll } else { me.0.feed.cfg.idle_poll }).await;
            }
        });
        Router::builder(endpoint).accept(ALPN, self.clone()).spawn()
    }

    /// Re-reads the node's tip (served in `Hello` and announcements).
    pub async fn refresh_tip(&self) {
        if let Some(t) = self.0.feed.refresh().await {
            *self.0.tip.write().unwrap() = (t.height, t.hash);
        }
    }
    /// The tip the subscribers see now.
    pub fn tip_info(&self) -> Option<TipInfo> {
        self.0.feed.current()
    }
    /// The shared tip feed (also what an SSE listener serves: [`crate::sse`]).
    pub fn feed(&self) -> Arc<TipFeed> {
        self.0.feed.clone()
    }
    /// Tip subscriptions open right now (every transport).
    pub fn subscribers(&self) -> usize {
        self.0.feed.subscribers()
    }
    pub fn tip(&self) -> (u64, [u8; 32]) {
        *self.0.tip.read().unwrap()
    }
    /// Whether this node advertises a relay (the relay controller flips it; the monthly cap clears it).
    pub fn set_relay_advertised(&self, on: bool) {
        *self.0.relay_advertised.write().unwrap() = on;
    }
    pub fn hello_log(&self) -> &HelloLog {
        &self.0.hello_log
    }
    pub fn stats(&self) -> &Stats {
        &self.0.stats
    }

    /// This node's own signed announcement.
    pub fn announcement(&self) -> Option<NodeAnnouncement> {
        let ep = self.0.endpoint.read().unwrap().clone()?;
        let mut flags = self.0.cfg.flags | node_flags::LC;
        if *self.0.relay_advertised.read().unwrap() {
            flags |= node_flags::RELAY;
        } else {
            flags &= !node_flags::RELAY;
        }
        Some(sign_announcement(&self.0.secret, &ep.addr(), self.0.cfg.https_rpc.clone(), self.tip().0, flags, self.0.cfg.version, now_secs()))
    }

    /// Remembers another server's announcement (verified, unexpired, at most [`MAX_NODES`]).
    pub fn add_known(&self, a: NodeAnnouncement) -> bool {
        let now = now_secs();
        if a.validate().is_err() || a.verify(now).is_err() || a.endpoint_id == *self.0.secret.public().as_bytes() {
            return false;
        }
        let mut k = self.0.known.lock().unwrap();
        k.retain(|x| !x.is_expired(now));
        if let Some(x) = k.iter_mut().find(|x| x.endpoint_id == a.endpoint_id) {
            // a fresher statement of the same node replaces the old one
            if a.expires > x.expires {
                *x = a;
            }
            return true;
        }
        if k.len() >= MAX_NODES as usize {
            // full of unexpired entries: keep what we know (fresh endpoint ids cost nothing, so a flood of them must not
            // push known servers out); a slot frees when one expires
            return false;
        }
        k.push(a);
        true
    }

    fn charge(&self, id: EndpointId, ip: Option<IpAddr>, cost: u32) -> Result<(), u32> {
        let l = &self.0.cfg.limits;
        let now = Instant::now();
        let mut retry = 0;
        {
            let mut m = self.0.peer_buckets.lock().unwrap();
            if m.len() > 20_000 {
                m.retain(|_, b| now.duration_since(b.last) < Duration::from_secs(60));
            }
            let b = m.entry(id).or_insert_with(|| Bucket::new(l.burst, now));
            if let Err(r) = b.take(cost as f64, l.burst, l.refill_per_sec, now) {
                retry = r;
            }
        }
        if let Some(ip) = ip {
            let mut m = self.0.ip_buckets.lock().unwrap();
            if m.len() > 20_000 {
                m.retain(|_, b| now.duration_since(b.last) < Duration::from_secs(60));
            }
            // Endpoint ids are free to make, so the IP's budget is a little larger but shared by all of them.
            let b = m.entry(ip).or_insert_with(|| Bucket::new(l.burst * 2.0, now));
            if let Err(r) = b.take(cost as f64, l.burst * 2.0, l.refill_per_sec * 2.0, now) {
                retry = retry.max(r);
            }
        } else {
            // relayed: no IP to key on, and endpoint ids are free, so all of them share one (larger) budget
            let m = l.relayed_budget_mult;
            let mut g = self.0.relayed_bucket.lock().unwrap();
            let b = g.get_or_insert_with(|| Bucket::new(l.burst * m, now));
            if let Err(r) = b.take(cost as f64, l.burst * m, l.refill_per_sec * m, now) {
                retry = retry.max(r);
            }
        }
        if retry > 0 { Err(retry) } else { Ok(()) }
    }

    /// Charges the size of an answer after the fact (see [`answer_cost`]) to the same budgets as [`Self::charge`].
    fn debit(&self, id: EndpointId, ip: Option<IpAddr>, cost: f64) {
        if cost <= 0.0 {
            return;
        }
        let l = &self.0.cfg.limits;
        if let Some(b) = self.0.peer_buckets.lock().unwrap().get_mut(&id) {
            b.debit(cost, l.burst);
        }
        match ip {
            Some(ip) => {
                if let Some(b) = self.0.ip_buckets.lock().unwrap().get_mut(&ip) {
                    b.debit(cost, l.burst * 2.0);
                }
            }
            None => {
                if let Some(b) = self.0.relayed_bucket.lock().unwrap().as_mut() {
                    b.debit(cost, l.burst * l.relayed_budget_mult);
                }
            }
        }
    }

    /// Answers one decoded request.
    async fn respond(&self, conn: &Connection, req: Request, hello_done: &mut bool) -> (Response, bool) {
        let cfg = &self.0.cfg;
        if let Err(e) = exec::validate(&req) {
            return (Response::Error(e), false);
        }
        match &req {
            Request::Hello { network, .. } => {
                if *network != cfg.network {
                    return (Response::Error(ErrorKind::BadHello), true);
                }
                *hello_done = true;
                self.0.hello_log.record(conn.remote_id());
                let (height, tip) = self.tip();
                let relay_url = self.announcement().and_then(|a| a.relay_url);
                (Response::Hello { minor: MINOR, features: features::SERVED, network: cfg.network, height, tip, relay_url }, false)
            }
            _ if !*hello_done => (Response::Error(ErrorKind::BadHello), false),
            Request::GetNodes { max } => {
                let mut v: Vec<_> = self.announcement().into_iter().collect();
                let now = now_secs();
                v.extend(self.0.known.lock().unwrap().iter().filter(|a| !a.is_expired(now)).cloned());
                v.truncate(*max as usize);
                (Response::Nodes(v), false)
            }
            Request::Probe { tcp_port } => (self.probe(conn, *tcp_port).await, false),
            _ => {
                if self.0.upstream_busy.fetch_add(1, Ordering::AcqRel) >= cfg.limits.max_upstream_concurrent {
                    self.0.upstream_busy.fetch_sub(1, Ordering::AcqRel);
                    return (Response::Error(ErrorKind::Busy { retry_ms: 500 }), false);
                }
                let t = if req.is_bulk() { cfg.limits.bulk_timeout } else { cfg.limits.upstream_timeout };
                let r = exec::run(&*self.0.upstream, &req, t).await;
                self.0.upstream_busy.fetch_sub(1, Ordering::AcqRel);
                (r, false)
            }
        }
    }

    async fn probe(&self, conn: &Connection, port: u16) -> Response {
        if port == 0 {
            return Response::Error(ErrorKind::Malformed);
        }
        let ip = observed_ip(conn);
        let Some(ip) = ip else {
            return Response::Probe { observed: None, reachable: false };
        };
        let ok = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect((ip, port))).await.is_ok_and(|r| r.is_ok());
        Response::Probe { observed: Some(ip.to_string()), reachable: ok }
    }

    /// A tip subscription: pushes the current tip, then every change, and a keepalive while nothing changes, until the client
    /// stops the stream or a write fails. The only thing it costs the node is the shared watcher's poll.
    async fn subscribe(&self, conn: &Connection, mut send: iroh::endpoint::SendStream, hello_done: bool) -> Result<bool, ()> {
        let l = &self.0.cfg.limits;
        let id = conn.remote_id();
        if !hello_done {
            let _ = write_frame(&mut send, &Response::Error(ErrorKind::BadHello), MAX_FRAME).await;
            let _ = send.finish();
            return Ok(false);
        }
        let Some(_sub) = self.0.feed.acquire(&id.to_string(), l.max_subs_per_endpoint) else {
            self.0.stats.subs_refused.fetch_add(1, Ordering::Relaxed);
            let _ = write_frame(&mut send, &Response::Error(ErrorKind::Busy { retry_ms: 30_000 }), MAX_FRAME).await;
            let _ = send.finish();
            return Ok(false);
        };
        self.0.stats.subscriptions.fetch_add(1, Ordering::Relaxed);
        let mut rx = self.0.feed.subscribe();
        let mut last: Option<[u8; 32]> = None;
        loop {
            let cur = rx.borrow_and_update().clone();
            if let Some(t) = cur.filter(|t| Some(t.hash) != last) {
                last = Some(t.hash);
                // the clock at sending, not when the watcher first saw the tip: a client corrects its own clock by it
                let t = TipInfo { server_time: now_secs(), ..t };
                if write_frame(&mut send, &Response::TipPush(t), MAX_FRAME).await.is_err() {
                    break;
                }
                self.0.stats.tips_pushed.fetch_add(1, Ordering::Relaxed);
            }
            tokio::select! {
                r = rx.changed() => if r.is_err() { break },
                _ = tokio::time::sleep(l.keepalive) => {
                    if write_frame(&mut send, &Response::Keepalive, MAX_FRAME).await.is_err() { break }
                }
                _ = send.stopped() => break,
            }
        }
        let _ = send.finish();
        Ok(false)
    }

    async fn handle_stream(&self, conn: &Connection, mut send: iroh::endpoint::SendStream, mut recv: iroh::endpoint::RecvStream, hello_done: &mut bool, malformed: &mut u32) -> Result<bool, ()> {
        let l = &self.0.cfg.limits;
        let ip = observed_ip(conn);
        let frame = match tokio::time::timeout(l.request_timeout, read_frame(&mut recv, MAX_REQUEST_FRAME)).await {
            Ok(Ok(Some(f))) => f,
            Ok(Ok(None)) | Err(_) => return Ok(false),
            Ok(Err(FrameError::TooLarge(..))) => {
                let _ = write_frame(&mut send, &Response::Error(ErrorKind::TooLarge), MAX_FRAME).await;
                let _ = send.finish();
                return Ok(true);
            }
            Ok(Err(_)) => return Ok(false),
        };
        self.0.stats.requests.fetch_add(1, Ordering::Relaxed);
        let req: Request = match decode_body(&frame) {
            Ok(r) => r,
            // a request from a newer minor: answered, not a protocol violation
            Err(_) if is_unknown_request(&frame) => {
                let _ = write_frame(&mut send, &Response::Error(ErrorKind::Unsupported), MAX_FRAME).await;
                let _ = send.finish();
                return Ok(false);
            }
            Err(_) => {
                self.0.stats.malformed.fetch_add(1, Ordering::Relaxed);
                *malformed += 1;
                let _ = write_frame(&mut send, &Response::Error(ErrorKind::Malformed), MAX_FRAME).await;
                let _ = send.finish();
                return Ok(*malformed >= l.max_malformed);
            }
        };
        if let Err(retry_ms) = self.charge(conn.remote_id(), ip, req.cost()) {
            self.0.stats.busy.fetch_add(1, Ordering::Relaxed);
            let _ = write_frame(&mut send, &Response::Error(ErrorKind::Busy { retry_ms }), MAX_FRAME).await;
            let _ = send.finish();
            return Ok(false);
        }
        if matches!(req, Request::SubscribeTip) {
            return self.subscribe(conn, send, *hello_done).await;
        }
        let (resp, close) = self.respond(conn, req, hello_done).await;
        let resp = match encode_frame(&resp, MAX_FRAME) {
            Ok(f) => f,
            Err(_) => encode_frame(&Response::Error(ErrorKind::TooLarge), MAX_FRAME).expect("small"),
        };
        self.debit(conn.remote_id(), ip, answer_cost(resp.len()));
        let _ = send.write_all(&resp).await;
        let _ = send.finish();
        // Let the answer leave before a closing connection drops it.
        if close {
            let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
        }
        Ok(close)
    }
}

/// The peer's IP if a direct path is open (None while only relayed: relayed peers share the relay's IP).
pub fn observed_ip(conn: &Connection) -> Option<IpAddr> {
    conn.paths().iter().find_map(|p| match p.remote_addr() {
        TransportAddr::Ip(a) if p.is_selected() => Some(a.ip()),
        _ => None,
    }).or_else(|| conn.paths().iter().find_map(|p| if let TransportAddr::Ip(a) = p.remote_addr() { Some(a.ip()) } else { None }))
}

impl ProtocolHandler for LcService {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let cfg = self.0.cfg.limits.clone();
        if self.0.conns.fetch_add(1, Ordering::AcqRel) >= cfg.max_connections {
            self.0.conns.fetch_sub(1, Ordering::AcqRel);
            conn.close(1u32.into(), b"server full");
            return Ok(());
        }
        struct Guard(Arc<Inner>, Option<IpAddr>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.conns.fetch_sub(1, Ordering::AcqRel);
                if let Some(ip) = self.1 {
                    let mut m = self.0.ip_conns.lock().unwrap();
                    if let Some(n) = m.get_mut(&ip) {
                        *n = n.saturating_sub(1);
                        if *n == 0 {
                            m.remove(&ip);
                        }
                    }
                }
            }
        }
        let ip = observed_ip(&conn);
        if let Some(ip) = ip {
            let mut m = self.0.ip_conns.lock().unwrap();
            let n = m.entry(ip).or_insert(0);
            if *n >= cfg.max_conns_per_ip {
                drop(m);
                self.0.conns.fetch_sub(1, Ordering::AcqRel);
                conn.close(1u32.into(), b"too many connections from this address");
                return Ok(());
            }
            *n += 1;
        }
        let _g = Guard(self.0.clone(), ip);
        self.0.stats.connections.fetch_add(1, Ordering::Relaxed);
        conn.set_max_concurrent_bi_streams(MAX_STREAMS_PER_CONN.into());
        conn.set_max_concurrent_uni_streams(0u32.into());
        let hello_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let malformed = Arc::new(AtomicU64::new(0));
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            let (send, recv) = tokio::select! {
                r = conn.accept_bi() => match r { Ok(s) => s, Err(_) => break },
                // reap finished stream tasks; a task that asked to close ends the connection
                Some(done) = tasks.join_next(), if !tasks.is_empty() => {
                    if matches!(done, Ok(true)) { conn.close(2u32.into(), b"protocol violation"); break; }
                    continue;
                }
            };
            let (me, c, hd, mal) = (self.clone(), conn.clone(), hello_done.clone(), malformed.clone());
            tasks.spawn(async move {
                let mut h = hd.load(Ordering::Acquire);
                let mut m = mal.load(Ordering::Acquire) as u32;
                let r = me.handle_stream(&c, send, recv, &mut h, &mut m).await;
                hd.store(h, Ordering::Release);
                mal.store(m as u64, Ordering::Release);
                matches!(r, Ok(true))
            });
        }
        tasks.abort_all();
        conn.closed().await;
        Ok(())
    }
}
