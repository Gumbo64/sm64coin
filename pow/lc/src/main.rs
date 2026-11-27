//! `sm64coin-lc`: the light-client service next to a node (and a tiny `ping` client).
//!
//!   sm64coin-lc serve --node-rpc http://127.0.0.1:16481 --data-dir ~/.sm64coin-lc --relay auto
//!   sm64coin-lc ping iroh:<endpoint id hex>@<relay url> --node-rpc http://127.0.0.1:16481
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use iroh::{RelayUrl, SecretKey};
use sm64coin_lc::client::{Client, Connector, IrohConnector, READ_TIMEOUT, SystemEnv};
use sm64coin_lc::endpoint::{Discovery, EndpointOptions, build_endpoint};
use sm64coin_lc::exec::{self, Upstream};
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::nodes::{NodeList, Source, parse_node_spec};
use sm64coin_lc::peers::{ClientPeers, gossip_once};
use sm64coin_lc::proto::{ALPN, Request, Response, node_flags};
use sm64coin_lc::relay::{HelloPolicy, RelayChoice, RelayConfig, RelayController, TlsMode};
use sm64coin_lc::server::{HelloLog, LcService, Limits, ServerConfig};

#[derive(Parser)]
#[command(name = "sm64coin-lc", version, about = "SM64Coin light-client network service")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Disc {
    None,
    N0,
    Dht,
}

#[derive(Clone, Copy, ValueEnum)]
enum Tls {
    SelfSigned,
    Pem,
    Acme,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the light-client protocol from the local node's RPC (and relay, if enabled).
    Serve {
        /// The local node's RESTRICTED RPC (sm64coind --rpc-restricted-bind-port 16489; on 127.0.0.1 it needs no opening).
        /// Only allowlisted methods are forwarded, and a restricted listener also caps request sizes and keeps sensitive pool
        /// data (Dandelion++ stem transactions) out of the answers.
        #[arg(long, default_value = "http://127.0.0.1:16489")]
        node_rpc: String,
        /// Serve from an unrestricted RPC anyway (closed test networks only).
        #[arg(long)]
        allow_unrestricted_rpc: bool,
        /// Holds secret.key, nodes.json, relay-usage.json, the ACME cache.
        #[arg(long, default_value = "sm64coin-lc-data")]
        data_dir: PathBuf,
        /// Relay: on, off, or auto (on after the reachability self-test).
        #[arg(long, default_value = "auto")]
        relay: RelayChoice,
        #[arg(long, default_value = "0.0.0.0:443")]
        relay_bind: SocketAddr,
        /// Serve the tip over plain HTTP (Server-Sent Events, GET /lc/tip) on this address, e.g. 127.0.0.1:16490; the public HTTPS
        /// front is the node's reverse proxy (see pow/lc/src/sse.rs). Off by default.
        #[arg(long)]
        sse_bind: Option<SocketAddr>,
        /// Read the client IP of an SSE subscriber from X-Forwarded-For (only when the listener is reachable only through that proxy;
        /// a loopback --sse-bind does this anyway).
        #[arg(long)]
        sse_trust_forwarded: bool,
        #[arg(long)]
        relay_quic_bind: Option<SocketAddr>,
        /// Per-client relay rate limit, KiB/s.
        #[arg(long, default_value_t = 256)]
        relay_rate_kib: u32,
        #[arg(long, default_value_t = 200)]
        relay_max_clients: usize,
        /// Monthly relay byte cap in GiB (0 = none).
        #[arg(long, default_value_t = 2048)]
        relay_monthly_cap_gib: u64,
        /// Domain or IP clients use to reach the relay (default: the IP peers see).
        #[arg(long)]
        public_host: Option<String>,
        #[arg(long, value_enum, default_value = "self-signed")]
        tls: Tls,
        #[arg(long)]
        tls_cert: Option<PathBuf>,
        #[arg(long)]
        tls_key: Option<PathBuf>,
        /// Let's Encrypt domain (repeatable). Implies --tls acme.
        #[arg(long)]
        acme_domain: Vec<String>,
        #[arg(long)]
        acme_contact: Vec<String>,
        /// Use the production CA (default: staging).
        #[arg(long)]
        acme_production: bool,
        #[arg(long, value_enum, default_value = "none")]
        discovery: Disc,
        /// SM64Coin relay URLs to use (repeatable), tried before n0's.
        #[arg(long)]
        relay_url: Vec<RelayUrl>,
        /// Never fall back to n0's public relays.
        #[arg(long)]
        no_n0_fallback: bool,
        /// Known servers to learn peers from: `iroh:<id>[@<relay>]` or https URLs (repeatable).
        #[arg(long)]
        bootstrap: Vec<String>,
        /// Public https JSON-RPC front of this node, if any (announced).
        #[arg(long)]
        https_rpc: Option<String>,
        #[arg(long)]
        bind: Option<SocketAddr>,
    },
    /// Say Hello to a server and print its tip.
    Ping {
        /// `iroh:<endpoint id hex>[@<relay url>]`
        server: String,
        /// A node whose genesis hash defines the network.
        #[arg(long, default_value = "http://127.0.0.1:16481")]
        node_rpc: String,
    },
}

fn load_or_create_key(dir: &PathBuf) -> std::io::Result<SecretKey> {
    std::fs::create_dir_all(dir)?;
    let p = dir.join("secret.key");
    if let Ok(b) = std::fs::read(&p) {
        if let Ok(a) = <[u8; 32]>::try_from(b.as_slice()) {
            return Ok(SecretKey::from_bytes(&a));
        }
    }
    let k = SecretKey::generate();
    std::fs::write(&p, k.to_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(k)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    match Cli::parse().cmd {
        Cmd::Serve { node_rpc, allow_unrestricted_rpc, data_dir, relay, relay_bind, sse_bind, sse_trust_forwarded, relay_quic_bind, relay_rate_kib, relay_max_clients, relay_monthly_cap_gib, public_host, tls, tls_cert, tls_key, acme_domain, acme_contact, acme_production, discovery, relay_url, no_n0_fallback, bootstrap, https_rpc, bind } => {
            let upstream: Arc<dyn Upstream> = Arc::new(HttpUpstream::new(&node_rpc)?);
            let network = loop {
                match exec::genesis_hash(&*upstream, Duration::from_secs(5)).await {
                    Ok(g) => break g,
                    Err(e) => {
                        tracing::warn!("waiting for the node at {node_rpc}: {e}");
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                }
            };
            match exec::node_restricted(&*upstream, Duration::from_secs(5)).await {
                Some(true) => {}
                _ if allow_unrestricted_rpc => tracing::warn!("serving from the UNRESTRICTED RPC at {node_rpc} (--allow-unrestricted-rpc)"),
                _ => return Err(format!("{node_rpc} is not a restricted RPC: start sm64coind with --rpc-restricted-bind-port 16489 and use --node-rpc http://127.0.0.1:16489").into()),
            }
            let secret = load_or_create_key(&data_dir)?;
            let hello = HelloLog::default();
            let ep = build_endpoint(EndpointOptions {
                secret_key: Some(secret.clone()),
                relays: relay_url,
                n0_fallback: !no_n0_fallback,
                discovery: match discovery {
                    Disc::None => Discovery::None,
                    Disc::N0 => Discovery::N0,
                    Disc::Dht => Discovery::Dht,
                },
                bind_addr: bind,
                alpns: vec![ALPN.to_vec()],
                ..Default::default()
            })
            .await?;
            let cfg = ServerConfig { network, limits: Limits::default(), flags: node_flags::ARCHIVE, version: 1, https_rpc };
            let service = LcService::new(cfg, upstream, secret, hello.clone());
            let router = service.serve(ep.clone());
            if let Some(bind) = sse_bind {
                let (a, _task) = sm64coin_lc::sse::serve(service.feed(), {
                    let mut c = sm64coin_lc::sse::SseConfig::behind_local_proxy(bind);
                    c.trust_forwarded |= sse_trust_forwarded;
                    c
                }).await?;
                println!("tip stream (SSE): http://{a}/lc/tip");
                std::mem::forget(_task);
            }
            println!("endpoint id: {}", ep.id());

            let mut list = std::fs::read_to_string(data_dir.join("nodes.json")).ok().and_then(|s| NodeList::from_json(&s).ok()).unwrap_or_default();
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
            for b in &bootstrap {
                if let Some(a) = parse_node_spec(b) {
                    list.add(a, Source::Bootstrap, now);
                }
            }
            let connector: Arc<dyn Connector> = Arc::new(IrohConnector::new(ep.clone(), network));
            let client = Arc::new(Client::new(list, connector.clone(), Arc::new(SystemEnv)));

            let tls_mode = if !acme_domain.is_empty() || matches!(tls, Tls::Acme) {
                TlsMode::Acme { domains: acme_domain, contact: acme_contact, cache_dir: data_dir.join("acme"), production: acme_production }
            } else if matches!(tls, Tls::Pem) {
                TlsMode::Manual { cert_pem: tls_cert.ok_or("--tls-cert needed")?, key_pem: tls_key.ok_or("--tls-key needed")? }
            } else {
                TlsMode::SelfSigned
            };
            let rcfg = RelayConfig {
                https_bind: relay_bind,
                quic_bind: relay_quic_bind,
                tls: tls_mode,
                bytes_per_sec: relay_rate_kib * 1024,
                max_clients: relay_max_clients,
                monthly_cap_bytes: (relay_monthly_cap_gib > 0).then(|| relay_monthly_cap_gib << 30),
                hello_policy: HelloPolicy::Probation { slots: 40, secs: 60 },
                data_dir: Some(data_dir.clone()),
                ..Default::default()
            };
            let peers = Arc::new(ClientPeers { client: client.clone(), connector, me: ep.id().to_string() });
            tokio::spawn(RelayController::new(relay, rcfg, public_host, hello, service.clone(), ep.clone(), peers).run());

            let (c2, s2, dir) = (client.clone(), service.clone(), data_dir.clone());
            tokio::spawn(async move {
                loop {
                    let n = gossip_once(&c2, &s2, 3).await;
                    tracing::info!(learned = n, "gossip");
                    let _ = std::fs::write(dir.join("nodes.json"), c2.list_json());
                    tokio::time::sleep(Duration::from_secs(600)).await;
                }
            });
            tokio::signal::ctrl_c().await?;
            router.shutdown().await?;
        }
        Cmd::Ping { server, node_rpc } => {
            let up = HttpUpstream::new(&node_rpc)?;
            let network = exec::genesis_hash(&up, Duration::from_secs(5)).await?;
            let ep = build_endpoint(EndpointOptions { discovery: Discovery::N0, ..Default::default() }).await?;
            let mut list = NodeList::new();
            list.add(parse_node_spec(&server).ok_or("bad server spec")?, Source::Manual, 0);
            let client = Client::new(list, Arc::new(IrohConnector::new(ep, network)), Arc::new(SystemEnv));
            match client.read(&Request::GetTip, READ_TIMEOUT).await? {
                (k, Response::Tip(t)) => println!("{k}: height {} tip {} difficulty {}", t.height, hex::encode(t.hash), t.cumulative_difficulty),
                (_, other) => println!("unexpected: {other:?}"),
            }
        }
    }
    Ok(())
}
