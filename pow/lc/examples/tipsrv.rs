//! A tip-push server for tests: a local relay (self-signed, loopback) and the light-client service in front of a node's RPC.
//! Prints one JSON line `{"id": <endpoint id>, "relay": <relay url>, "genesis": <hex>}` and serves until killed.
//!
//!   cargo run --example tipsrv --features relay -- http://127.0.0.1:16481 [--poll-ms 1000]
//!
//! Never for the real network: the relay's certificate is self-signed and admits everybody.
use std::sync::Arc;
use std::time::Duration;

use iroh::SecretKey;
use sm64coin_lc::endpoint::{EndpointOptions, build_endpoint};
use sm64coin_lc::exec::Upstream;
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::proto::ALPN;
use sm64coin_lc::relay::{HelloPolicy, RelayConfig, RelayHandle, TlsMode};
use sm64coin_lc::server::{HelloLog, Limits, LcService, ServerConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let node = args.next().ok_or("usage: tipsrv <node rpc url> [--poll-ms N]")?;
    let mut limits = Limits::default();
    while let Some(a) = args.next() {
        if a == "--poll-ms" {
            limits.tip_poll = Duration::from_millis(args.next().ok_or("--poll-ms N")?.parse()?);
        }
    }
    let up = Arc::new(HttpUpstream::new(&node)?);
    let network = sm64coin_lc::exec::genesis_hash(&*up, Duration::from_secs(10)).await?;
    let relay = RelayHandle::spawn(&RelayConfig { https_bind: ([127, 0, 0, 1], 0).into(), tls: TlsMode::SelfSigned, hello_policy: HelloPolicy::Off, ..Default::default() }, HelloLog::default()).await?;
    let relay_url = format!("https://127.0.0.1:{}", relay.https_addr().unwrap().port());
    let secret = SecretKey::generate();
    let ep = build_endpoint(EndpointOptions {
        secret_key: Some(secret.clone()),
        relays: vec![relay_url.parse()?],
        only_trust_certs: vec![relay.self_signed_cert.clone().unwrap()],
        n0_fallback: false,
        alpns: vec![ALPN.to_vec()],
        ..Default::default()
    })
    .await?;
    let svc = LcService::new(ServerConfig { network, limits, flags: 0, version: 1, https_rpc: None }, up as Arc<dyn Upstream>, secret, HelloLog::default());
    svc.refresh_tip().await;
    let _router = svc.serve(ep.clone());
    println!("{}", serde_json::json!({"id": ep.id().to_string(), "relay": relay_url, "genesis": hex::encode(network)}));
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let st = svc.stats();
        let n = std::sync::atomic::Ordering::Relaxed;
        eprintln!("STATS {}", serde_json::json!({"subscribers": svc.subscribers(), "tips_pushed": st.tips_pushed.load(n), "requests": st.requests.load(n), "connections": st.connections.load(n)}));
    }
}
