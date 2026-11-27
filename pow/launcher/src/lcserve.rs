//! `--lc`: the light-client service (sm64coin-lc: `LcService` on an iroh endpoint, plus the `RelayController` in `auto` mode)
//! run in this process next to the node the launcher started. The same code as the standalone `sm64coin-lc serve`
//! (pow/lc/src/main.rs), which is what a node run by hand or by systemd uses (packaging/sm64coin-lc.service).
//! Runs on its own thread with its own tokio runtime; nothing here touches the node except its restricted-safe RPC on
//! 127.0.0.1 (only allowlisted methods are ever forwarded, see `sm64coin_lc::allowlist`).
use sm64coin_lc::client::{Client, Connector, IrohConnector, SystemEnv};
use sm64coin_lc::endpoint::{Discovery, EndpointOptions, build_endpoint};
use sm64coin_lc::exec::{self, Upstream};
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::nodes::{NodeList, Source, parse_node_spec};
use sm64coin_lc::peers::{ClientPeers, gossip_once};
use sm64coin_lc::proto::{ALPN, node_flags};
use sm64coin_lc::relay::{HelloPolicy, RelayChoice, RelayConfig, RelayController, TlsMode};
use sm64coin_lc::server::{HelloLog, LcService, Limits, ServerConfig};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct LcConfig {
    /// The local node's RPC port (127.0.0.1).
    pub node_rpc_port: u16,
    /// `secret.key`, `nodes.json`, `relay-usage.json` live here.
    pub data_dir: PathBuf,
    pub relay: RelayChoice,
    /// Where the embedded relay listens when it is on (port 443 needs privileges on Linux: a failed bind keeps it off).
    pub relay_bind: SocketAddr,
    pub discovery: Discovery,
    /// `iroh:<id>[@relay]` or https URLs of servers to learn peers from.
    pub bootstrap: Vec<String>,
    /// Use n0's public relays (the default; tests and closed networks turn it off).
    pub n0_fallback: bool,
}

impl LcConfig {
    pub fn new(node_rpc_port: u16, data_dir: PathBuf) -> Self {
        LcConfig { node_rpc_port, data_dir, relay: RelayChoice::Auto, relay_bind: "0.0.0.0:443".parse().unwrap(), discovery: Discovery::Dht, bootstrap: vec![], n0_fallback: true }
    }
}

/// A running service; dropping it (or `stop`) ends the thread's runtime.
pub struct LcHandle {
    stop: Arc<AtomicBool>,
}

impl LcHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for LcHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn load_or_create_key(dir: &Path) -> std::io::Result<iroh::SecretKey> {
    std::fs::create_dir_all(dir)?;
    let p = dir.join("secret.key");
    if let Ok(b) = std::fs::read(&p) {
        if let Ok(a) = <[u8; 32]>::try_from(b.as_slice()) {
            return Ok(iroh::SecretKey::from_bytes(&a));
        }
    }
    let k = iroh::SecretKey::generate();
    std::fs::write(&p, k.to_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(k)
}

/// Starts the service on a background thread; returns at once (the thread waits for the node's RPC itself).
pub fn start(cfg: LcConfig) -> Result<LcHandle, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    std::thread::Builder::new()
        .name("sm64coin-lc".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(r) => r,
                Err(e) => return eprintln!("light-client service: no runtime: {e}"),
            };
            if let Err(e) = rt.block_on(run(cfg, s2)) {
                eprintln!("light-client service stopped: {e}");
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(LcHandle { stop })
}

async fn run(cfg: LcConfig, stop: Arc<AtomicBool>) -> Result<(), Box<dyn std::error::Error>> {
    let upstream: Arc<dyn Upstream> = Arc::new(HttpUpstream::new(&format!("http://127.0.0.1:{}", cfg.node_rpc_port))?);
    let network = loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match exec::genesis_hash(&*upstream, Duration::from_secs(5)).await {
            Ok(g) => break g,
            Err(_) => tokio::time::sleep(Duration::from_secs(3)).await,
        }
    };
    let secret = load_or_create_key(&cfg.data_dir)?;
    let hello = HelloLog::default();
    let ep = build_endpoint(EndpointOptions { secret_key: Some(secret.clone()), discovery: cfg.discovery, n0_fallback: cfg.n0_fallback, alpns: vec![ALPN.to_vec()], ..Default::default() }).await?;
    let service = LcService::new(ServerConfig { network, limits: Limits::default(), flags: node_flags::ARCHIVE, version: 1, https_rpc: None }, upstream, secret, hello.clone());
    let router = service.serve(ep.clone());
    println!("Light-client service: endpoint id {} (relay {:?}).", ep.id(), cfg.relay);

    let mut list = std::fs::read_to_string(cfg.data_dir.join("nodes.json")).ok().and_then(|s| NodeList::from_json(&s).ok()).unwrap_or_default();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    for b in &cfg.bootstrap {
        if let Some(a) = parse_node_spec(b) {
            list.add(a, Source::Bootstrap, now);
        }
    }
    let connector: Arc<dyn Connector> = Arc::new(IrohConnector::new(ep.clone(), network));
    let client = Arc::new(Client::new(list, connector.clone(), Arc::new(SystemEnv)));
    let rcfg = RelayConfig {
        https_bind: cfg.relay_bind,
        tls: TlsMode::SelfSigned,
        hello_policy: HelloPolicy::Probation { slots: 40, secs: 60 },
        data_dir: Some(cfg.data_dir.clone()),
        ..Default::default()
    };
    let peers = Arc::new(ClientPeers { client: client.clone(), connector, me: ep.id().to_string() });
    tokio::spawn(RelayController::new(cfg.relay, rcfg, None, hello, service.clone(), ep.clone(), peers).run());
    let (c2, s2, dir, stop2) = (client.clone(), service.clone(), cfg.data_dir.clone(), stop.clone());
    tokio::spawn(async move {
        while !stop2.load(Ordering::SeqCst) {
            let n = gossip_once(&c2, &s2, 3).await;
            let _ = n;
            let _ = std::fs::write(dir.join("nodes.json"), c2.list_json());
            tokio::time::sleep(Duration::from_secs(600)).await;
        }
    });
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    router.shutdown().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// The service starts against a (fake) node RPC, makes its key and stops; no network (no discovery, no relays, no n0).
    #[test]
    fn starts_next_to_a_node_and_stops() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let hash = "ab".repeat(32);
        std::thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let mut b = String::new();
                let _ = req.as_reader().read_to_string(&mut b);
                let body = serde_json::json!({"jsonrpc": "2.0", "id": 0, "result": {"block_header": {"hash": hash}, "status": "OK"}}).to_string();
                let _ = req.respond(tiny_http::Response::from_string(body));
            }
        });
        let dir = std::env::temp_dir().join(format!("lcserve-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut c = LcConfig::new(port, dir.clone());
        (c.relay, c.discovery, c.n0_fallback, c.relay_bind) = (RelayChoice::Off, Discovery::None, false, "127.0.0.1:0".parse().unwrap());
        let h = start(c).unwrap();
        let t = std::time::Instant::now();
        while !dir.join("secret.key").is_file() {
            assert!(t.elapsed() < Duration::from_secs(20), "the service did not come up");
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(std::fs::read(dir.join("secret.key")).unwrap().len(), 32);
        h.stop();
        std::thread::sleep(Duration::from_millis(1500));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
