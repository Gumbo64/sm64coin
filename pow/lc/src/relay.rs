//! The embedded iroh-relay server: limits, access control, TLS (self-signed for tests, PEM files,
//! or Let's Encrypt), the monthly byte cap, and the reachability self-test that decides whether a
//! node relays at all.
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::EndpointId;
use iroh_relay::server::{Access, AccessControl, CertConfig, ClientRateLimit, ClientRequest, ConnectionId, Limits, QuicConfig, RelayConfig as RelayServerConfig, Server, ServerConfig, TlsConfig};

use crate::client::Transport;
use crate::proto::{ErrorKind, Request, Response};
use crate::server::HelloLog;

/// Relay on/off/auto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayChoice {
    Off,
    On,
    /// On only after the reachability self-test passed.
    Auto,
}

impl std::str::FromStr for RelayChoice {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "off" => Ok(Self::Off),
            "on" => Ok(Self::On),
            "auto" => Ok(Self::Auto),
            o => Err(format!("relay must be on, off or auto, not {o}")),
        }
    }
}

#[derive(Clone, Debug)]
pub enum TlsMode {
    /// A fresh self-signed certificate for localhost / 127.0.0.1 (development and tests only).
    SelfSigned,
    /// PEM certificate chain and key files (read once; restart to reload).
    Manual { cert_pem: PathBuf, key_pem: PathBuf },
    /// Let's Encrypt (ACME tls-alpn-01 on the HTTPS port, which must be 443 and publicly reachable).
    Acme { domains: Vec<String>, contact: Vec<String>, cache_dir: PathBuf, production: bool },
}

/// Who may use the relay. The relay cannot tell an SM64Coin client from another iroh app when it
/// connects (a new client has to reach the relay *before* it can `Hello` the node over it), so the
/// default admits strangers on probation and drops those that never `Hello`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelloPolicy {
    /// Anyone (the relay is then a free relay for unrelated iroh apps).
    Off,
    /// Endpoints with a recent `Hello` are admitted; others use at most `slots` probation slots and are
    /// disconnected after `secs` unless they `Hello`.
    Probation { slots: usize, secs: u64 },
    /// Only endpoints with a `Hello` in the last hour (a relay co-located with the service for peers that already know it).
    Strict,
}

#[derive(Clone, Debug)]
pub struct RelayConfig {
    pub https_bind: SocketAddr,
    pub http_bind: Option<SocketAddr>,
    pub quic_bind: Option<SocketAddr>,
    pub tls: TlsMode,
    /// Per-client receive limit (default 256 KiB/s).
    pub bytes_per_sec: u32,
    /// Burst (default 1 MiB).
    pub burst_bytes: u32,
    /// Concurrent clients (default 200).
    pub max_clients: usize,
    /// Monthly byte cap (sent + received); after it new clients are refused and the node stops advertising RELAY.
    pub monthly_cap_bytes: Option<u64>,
    pub hello_policy: HelloPolicy,
    /// Where the monthly usage is kept (`relay-usage.json`); None = memory only.
    pub data_dir: Option<PathBuf>,
    /// Advertise a relay with a self-signed certificate in `Auto` mode. Off by default: no client trusts such a relay (iroh
    /// checks relay certificates against the web PKI, browsers too), so advertising it would only make the node harder to
    /// reach. Tests that pin the certificate turn it on.
    pub advertise_self_signed: bool,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            https_bind: SocketAddr::from(([0, 0, 0, 0], 443)),
            http_bind: None,
            quic_bind: None,
            tls: TlsMode::SelfSigned,
            bytes_per_sec: 256 * 1024,
            burst_bytes: 1 << 20,
            max_clients: 200,
            monthly_cap_bytes: Some(2 << 40),
            hello_policy: HelloPolicy::Probation { slots: 40, secs: 60 },
            data_dir: None,
            advertise_self_signed: false,
        }
    }
}

// ---------------------------------------------------------------- monthly usage

/// "YYYYMM" of a unix time (civil calendar, UTC).
pub fn month_id(unix_secs: u64) -> u32 {
    let z = (unix_secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y * 100 + m) as u32
}

/// Bytes relayed this calendar month, surviving restarts and month changes.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub month: u32,
    pub bytes: u64,
    #[serde(skip)]
    last_counter: u64,
}

impl Usage {
    /// Feeds the relay's lifetime byte counter (sent + received); returns this month's total.
    pub fn update(&mut self, counter_total: u64, now: u64) -> u64 {
        let m = month_id(now);
        if m != self.month {
            self.month = m;
            self.bytes = 0;
        }
        self.bytes += counter_total.saturating_sub(self.last_counter);
        self.last_counter = counter_total;
        self.bytes
    }
}

/// Writes `relay-usage.json` atomically (temp file + rename): a crash mid-write must not reset the month's count to zero
/// (an unreadable file is read as no usage, which would lift the cap).
pub fn save_usage(dir: &std::path::Path, u: &Usage) {
    let tmp = dir.join("relay-usage.json.tmp");
    if std::fs::write(&tmp, serde_json::to_string(u).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join("relay-usage.json"));
    }
}

// ---------------------------------------------------------------- access control

#[derive(Debug)]
struct GateState {
    clients: HashMap<ConnectionId, (EndpointId, Instant)>,
}

/// The relay's [`AccessControl`].
#[derive(Debug)]
pub struct Gate {
    hello: HelloLog,
    policy: HelloPolicy,
    max_clients: usize,
    over_cap: Arc<AtomicBool>,
    state: Mutex<GateState>,
}

impl Gate {
    pub fn new(hello: HelloLog, policy: HelloPolicy, max_clients: usize, over_cap: Arc<AtomicBool>) -> Self {
        Self { hello, policy, max_clients, over_cap, state: Mutex::new(GateState { clients: HashMap::new() }) }
    }
    pub fn clients(&self) -> usize {
        self.state.lock().unwrap().clients.len()
    }
    /// Connections on probation for longer than the policy allows.
    pub fn expired_probation(&self) -> Vec<(EndpointId, ConnectionId)> {
        let HelloPolicy::Probation { secs, .. } = self.policy else { return vec![] };
        self.state
            .lock()
            .unwrap()
            .clients
            .iter()
            .filter(|(_, (id, since))| since.elapsed() > Duration::from_secs(secs) && !self.hello.seen_within(id, Duration::from_secs(3600)))
            .map(|(c, (id, _))| (*id, *c))
            .collect()
    }
}

impl AccessControl for Gate {
    async fn on_connect(&self, request: &ClientRequest) -> Access {
        let id = request.endpoint_id();
        if self.over_cap.load(Ordering::Relaxed) {
            return Access::Deny { reason: Some("monthly byte cap reached".into()) };
        }
        let known = self.hello.seen_within(&id, Duration::from_secs(3600));
        let mut st = self.state.lock().unwrap();
        if st.clients.len() >= self.max_clients {
            return Access::Deny { reason: Some("relay full".into()) };
        }
        match self.policy {
            HelloPolicy::Off => {}
            HelloPolicy::Strict if !known => return Access::Deny { reason: Some("sm64coin clients only".into()) },
            HelloPolicy::Strict => {}
            HelloPolicy::Probation { slots, .. } => {
                if !known {
                    let strangers = st.clients.values().filter(|(i, _)| !self.hello.seen_within(i, Duration::from_secs(3600))).count();
                    if strangers >= slots {
                        return Access::Deny { reason: Some("sm64coin clients only".into()) };
                    }
                }
            }
        }
        st.clients.insert(request.connection_id(), (id, Instant::now()));
        Access::Allow
    }
    fn on_disconnect(&self, _endpoint_id: EndpointId, connection_id: ConnectionId) {
        self.state.lock().unwrap().clients.remove(&connection_id);
    }
}

// ---------------------------------------------------------------- the running relay

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("tls: {0}")]
    Tls(String),
    #[error("cannot start the relay: {0}")]
    Spawn(String),
}

/// A running relay. Dropping it stops it.
pub struct RelayHandle {
    server: Server,
    gate: Arc<Gate>,
    over_cap: Arc<AtomicBool>,
    /// DER of the self-signed certificate (`TlsMode::SelfSigned`), for clients that must trust it.
    pub self_signed_cert: Option<Vec<u8>>,
    usage: Arc<Mutex<Usage>>,
    data_dir: Option<PathBuf>,
    _sweeper: AbortOnDrop,
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn load_manual(cert: &PathBuf, key: &PathBuf) -> Result<rustls::ServerConfig, RelayError> {
    use rustls_pki_types::pem::PemObject;
    let certs = rustls_pki_types::CertificateDer::pem_file_iter(cert).map_err(|e| RelayError::Tls(e.to_string()))?.collect::<Result<Vec<_>, _>>().map_err(|e| RelayError::Tls(e.to_string()))?;
    let key = rustls_pki_types::PrivateKeyDer::from_pem_file(key).map_err(|e| RelayError::Tls(e.to_string()))?;
    rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| RelayError::Tls(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| RelayError::Tls(e.to_string()))
}

/// The iroh-relay certificate config for a [`TlsMode`] (public so ACME wiring is testable without a server).
pub fn cert_config(mode: &TlsMode) -> Result<(CertConfig, Option<Vec<u8>>), RelayError> {
    Ok(match mode {
        TlsMode::SelfSigned => {
            let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string()]).map_err(|e| RelayError::Tls(e.to_string()))?;
            let der = cert.cert.der().clone();
            let key = rustls_pki_types::PrivateKeyDer::from(rustls_pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()));
            let server_config = rustls::ServerConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .map_err(|e| RelayError::Tls(e.to_string()))?
                .with_no_client_auth()
                .with_single_cert(vec![der.clone()], key)
                .map_err(|e| RelayError::Tls(e.to_string()))?;
            (CertConfig::Manual { server_config }, Some(der.to_vec()))
        }
        TlsMode::Manual { cert_pem, key_pem } => (CertConfig::Manual { server_config: load_manual(cert_pem, key_pem)? }, None),
        TlsMode::Acme { domains, contact, cache_dir, production } => {
            if domains.is_empty() {
                return Err(RelayError::Tls("ACME needs at least one domain".into()));
            }
            let acme = iroh_relay::server::AcmeConfig::letsencrypt(*production).domains(domains.clone()).contact(contact.clone()).cache_path(cache_dir.clone());
            let builder = rustls::ServerConfig::builder_with_provider(provider()).with_safe_default_protocol_versions().map_err(|e| RelayError::Tls(e.to_string()))?.with_no_client_auth();
            (CertConfig::LetsEncrypt { acme_config: acme, server_config_builder: builder }, None)
        }
    })
}

impl RelayHandle {
    pub async fn spawn(cfg: &RelayConfig, hello: HelloLog) -> Result<Self, RelayError> {
        let over_cap = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(Gate::new(hello, cfg.hello_policy, cfg.max_clients, over_cap.clone()));
        let (cert, der) = cert_config(&cfg.tls)?;
        let mut relay = RelayServerConfig::new(cfg.http_bind.unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 0))));
        relay.tls = Some(TlsConfig::new(cfg.https_bind, cert));
        let mut limits = Limits::default();
        limits.client_rx = NonZeroU32::new(cfg.bytes_per_sec).map(|b| {
            let mut l = ClientRateLimit::new(b);
            l.max_burst_bytes = NonZeroU32::new(cfg.burst_bytes);
            l
        });
        relay.limits = limits;
        relay.key_cache_capacity = Some(1024);
        relay.access = gate.clone();
        let mut sc = ServerConfig::default();
        sc.relay = Some(relay);
        sc.quic = cfg.quic_bind.map(QuicConfig::new);
        let server = Server::spawn(sc).await.map_err(|e| RelayError::Spawn(e.to_string()))?;

        let usage = Arc::new(Mutex::new(cfg.data_dir.as_ref().and_then(|d| std::fs::read_to_string(d.join("relay-usage.json")).ok()).and_then(|s| serde_json::from_str::<Usage>(&s).ok()).unwrap_or_default()));
        let sweeper = {
            let (gate, over_cap, usage, cap, dir) = (gate.clone(), over_cap.clone(), usage.clone(), cfg.monthly_cap_bytes, cfg.data_dir.clone());
            let service = server.relay_service().cloned();
            let metrics = server.metrics().server.clone();
            tokio::spawn(async move {
                let mut tick = 0u64;
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    tick += 1;
                    if let Some(s) = &service {
                        for (id, conn) in gate.expired_probation() {
                            s.clients().disconnect(id, Some(conn));
                        }
                    }
                    let total = metrics.bytes_sent.get() + metrics.bytes_recv.get();
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                    let used = usage.lock().unwrap().update(total, now);
                    over_cap.store(cap.is_some_and(|c| used >= c), Ordering::Relaxed);
                    if tick % 12 == 0 {
                        if let Some(d) = &dir {
                            save_usage(d, &usage.lock().unwrap());
                        }
                    }
                }
            })
        };
        Ok(Self { server, gate, over_cap, self_signed_cert: der, usage, data_dir: cfg.data_dir.clone(), _sweeper: AbortOnDrop(sweeper) })
    }
    pub fn https_addr(&self) -> Option<SocketAddr> {
        self.server.https_addr()
    }
    pub fn quic_addr(&self) -> Option<SocketAddr> {
        self.server.quic_addr()
    }
    /// The URL clients would use for a relay at `host` (a domain or IP) on this relay's HTTPS port.
    pub fn url_for(&self, host: &str) -> Option<String> {
        let port = self.https_addr()?.port();
        Some(if host.contains(':') { format!("https://[{host}]:{port}") } else if port == 443 { format!("https://{host}") } else { format!("https://{host}:{port}") })
    }
    pub fn clients(&self) -> usize {
        self.gate.clients()
    }
    pub fn over_cap(&self) -> bool {
        self.over_cap.load(Ordering::Relaxed)
    }
    pub fn used_this_month(&self) -> u64 {
        self.usage.lock().unwrap().bytes
    }
    pub async fn shutdown(self) {
        if let Some(d) = &self.data_dir {
            save_usage(d, &self.usage.lock().unwrap());
        }
        let _ = self.server.shutdown().await;
    }
}

// ---------------------------------------------------------------- reachability self-test

/// One peer's answer to our `Probe`.
#[derive(Clone, Debug)]
pub struct ProbeOutcome {
    /// Address the peer is known at (for the /16 check), if direct.
    pub peer_ip: Option<IpAddr>,
    pub peer_id: String,
    pub observed: Option<IpAddr>,
    pub reachable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Reachable { observed: IpAddr },
    Unreachable(String),
}

/// The network a peer address is in, for "two different networks": IPv4 /16, IPv6 /32.
fn network_of(ip: IpAddr) -> [u8; 4] {
    match ip {
        IpAddr::V4(v) => [4, v.octets()[0], v.octets()[1], 0],
        IpAddr::V6(v) => {
            let o = v.octets();
            [o[0], o[1], o[2], o[3]]
        }
    }
}

/// True for addresses nobody on the internet can connect to.
pub fn is_non_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local() || v.is_unspecified() || (v.octets()[0] == 100 && (64..128).contains(&v.octets()[1])),
        IpAddr::V6(v) => v.is_loopback() || v.is_unspecified() || (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// Two reachable answers from different peers at known public addresses in different networks (IPv4 /16, IPv6 /32),
/// agreeing on the observed public (not private, not CGNAT) IP, enable the relay. A peer whose address is not known (only
/// relayed to us) does not count: two endpoint ids are free to make, two networks are not. Anything else keeps it off.
pub fn decide(outcomes: &[ProbeOutcome]) -> Verdict {
    let ok: Vec<&ProbeOutcome> = outcomes.iter().filter(|o| o.reachable && o.observed.is_some_and(|i| !is_non_public(i))).collect();
    let mut peers = std::collections::HashSet::new();
    let mut nets = std::collections::HashSet::new();
    let mut observed = None;
    for o in &ok {
        let ip = o.observed.unwrap();
        if observed.is_some_and(|x| x != ip) {
            return Verdict::Unreachable("peers see different public addresses".into());
        }
        observed = Some(ip);
        if let Some(p) = o.peer_ip.filter(|p| !is_non_public(*p)) {
            if nets.insert(network_of(p)) {
                peers.insert(o.peer_id.clone());
            }
        }
    }
    match observed {
        Some(observed) if peers.len() >= 2 && nets.len() >= 2 => Verdict::Reachable { observed },
        _ => Verdict::Unreachable(format!("{} of {} peers could connect back ({} from distinct public networks)", ok.len(), outcomes.len(), nets.len())),
    }
}

/// The public IPs this endpoint knows for itself (its own address discovery through the relays). The probes' observed IP
/// must be one of them when there are any, so peers cannot talk the node into advertising someone else's address.
pub fn own_public_ips(ep: &iroh::Endpoint) -> Vec<IpAddr> {
    ep.addr().addrs.iter().filter_map(|t| if let iroh::TransportAddr::Ip(a) = t { Some(a.ip()) } else { None }).filter(|i| !is_non_public(*i)).collect()
}

/// Asks each peer to connect back to us on `tcp_port`.
pub async fn probe_peers(peers: &[(String, Option<IpAddr>, Arc<dyn Transport>)], tcp_port: u16) -> Vec<ProbeOutcome> {
    let futs = peers.iter().map(|(id, ip, t)| async move {
        let r = t.request(&Request::Probe { tcp_port }, Duration::from_secs(15)).await;
        match r {
            // the address the connection to the peer actually uses beats any address the peer announced
            Ok(Response::Probe { observed, reachable }) => ProbeOutcome { peer_ip: t.remote_ip().or(*ip), peer_id: id.clone(), observed: observed.and_then(|o| o.parse().ok()), reachable },
            Ok(Response::Error(ErrorKind::Busy { .. })) | Ok(_) | Err(_) => ProbeOutcome { peer_ip: t.remote_ip().or(*ip), peer_id: id.clone(), observed: None, reachable: false },
        }
    });
    futures_util::future::join_all(futs).await
}

/// Re-test interval after a failure (and the refresh interval after a pass).
pub const RETEST: Duration = Duration::from_secs(6 * 3600);

// ---------------------------------------------------------------- enable / disable

/// Where the controller finds peers to probe through.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait PeerSource: Send + Sync {
    async fn peers(&self) -> Vec<(String, Option<IpAddr>, Arc<dyn Transport>)>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayState {
    Off,
    /// Running and advertised at this URL.
    Active { url: String },
    /// Auto mode: test failed, relay not running.
    Failed(String),
}

/// Decides whether this node relays: `Off` never, `On` always, `Auto` after a passed self-test
/// (re-run every [`RETEST`]).
pub struct RelayController {
    choice: RelayChoice,
    cfg: RelayConfig,
    /// Domain (or IP) clients should use; if None, the IP peers observed is used.
    public_host: Option<String>,
    hello: HelloLog,
    service: crate::server::LcService,
    endpoint: iroh::Endpoint,
    peers: Arc<dyn PeerSource>,
    handle: Option<RelayHandle>,
    state: RelayState,
}

impl RelayController {
    pub fn new(choice: RelayChoice, cfg: RelayConfig, public_host: Option<String>, hello: HelloLog, service: crate::server::LcService, endpoint: iroh::Endpoint, peers: Arc<dyn PeerSource>) -> Self {
        Self { choice, cfg, public_host, hello, service, endpoint, peers, handle: None, state: RelayState::Off }
    }
    pub fn state(&self) -> &RelayState {
        &self.state
    }
    pub fn handle(&self) -> Option<&RelayHandle> {
        self.handle.as_ref()
    }

    /// One decision cycle.
    pub async fn step(&mut self) -> &RelayState {
        // The monthly cap stops the advertisement (and the relay refuses new clients itself).
        if self.handle.as_ref().is_some_and(|h| h.over_cap()) {
            self.service.set_relay_advertised(false);
            self.state = RelayState::Failed("monthly byte cap reached".into());
            return &self.state;
        }
        match self.choice {
            RelayChoice::Off => {
                self.stop().await;
            }
            RelayChoice::On => {
                if self.handle.is_none() {
                    match RelayHandle::spawn(&self.cfg, self.hello.clone()).await {
                        Ok(h) => self.handle = Some(h),
                        Err(e) => {
                            self.state = RelayState::Failed(e.to_string());
                            return &self.state;
                        }
                    }
                }
                let host = self.public_host.clone().unwrap_or_else(|| "localhost".into());
                self.advertise(&host).await;
            }
            RelayChoice::Auto => {
                if matches!(self.cfg.tls, TlsMode::SelfSigned) && !self.cfg.advertise_self_signed {
                    self.stop().await;
                    self.state = RelayState::Failed("no browser-valid certificate: start with --tls acme (--acme-domain, --acme-production) or --tls pem".into());
                    return &self.state;
                }
                if self.handle.is_none() {
                    match RelayHandle::spawn(&self.cfg, self.hello.clone()).await {
                        Ok(h) => self.handle = Some(h),
                        Err(e) => {
                            self.state = RelayState::Failed(e.to_string());
                            return &self.state;
                        }
                    }
                }
                let port = self.handle.as_ref().and_then(|h| h.https_addr()).map(|a| a.port()).unwrap_or(0);
                let peers = self.peers.peers().await;
                let verdict = match decide(&probe_peers(&peers, port).await) {
                    Verdict::Reachable { observed } => {
                        let own = own_public_ips(&self.endpoint);
                        if own.is_empty() || own.contains(&observed) {
                            Verdict::Reachable { observed }
                        } else {
                            Verdict::Unreachable(format!("peers report {observed}, this endpoint knows itself as {own:?}"))
                        }
                    }
                    v => v,
                };
                match verdict {
                    Verdict::Reachable { observed } => {
                        let host = self.public_host.clone().unwrap_or_else(|| observed.to_string());
                        self.advertise(&host).await;
                    }
                    Verdict::Unreachable(why) => {
                        self.stop().await;
                        self.state = RelayState::Failed(why);
                    }
                }
            }
        }
        &self.state
    }

    async fn advertise(&mut self, host: &str) {
        let Some(url) = self.handle.as_ref().and_then(|h| h.url_for(host)) else { return };
        if let Ok(parsed) = url.parse::<iroh::RelayUrl>() {
            let quic = self.handle.as_ref().and_then(|h| h.quic_addr()).map(|a| iroh_relay::RelayQuicConfig::new(a.port()));
            self.endpoint.insert_relay(parsed.clone(), Arc::new(iroh_relay::RelayConfig::new(parsed, quic))).await;
        }
        self.service.set_relay_advertised(true);
        self.state = RelayState::Active { url };
    }

    async fn stop(&mut self) {
        self.service.set_relay_advertised(false);
        if let Some(h) = self.handle.take() {
            h.shutdown().await;
        }
        if !matches!(self.state, RelayState::Failed(_)) {
            self.state = RelayState::Off;
        }
    }

    /// `step` forever (a failed Auto test retries every [`RETEST`], a passed one refreshes as often).
    pub async fn run(mut self) {
        loop {
            let st = self.step().await.clone();
            tracing::info!(?st, "relay state");
            tokio::time::sleep(if matches!(self.choice, RelayChoice::Auto) { RETEST } else { Duration::from_secs(60) }).await;
        }
    }
}
