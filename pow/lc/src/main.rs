//! `sm64coin-lc`: the light-client service next to a node: the chain tip pushed over HTTP (Server-Sent Events, `GET /lc/tip`),
//! with a tiny `ping` client. The public HTTPS address is the node's reverse proxy (see `sse.rs`).
//!
//!   sm64coin-lc serve --node-rpc http://127.0.0.1:16489 --sse-bind 127.0.0.1:16490
//!   sm64coin-lc serve --tls-cert /var/lib/sm64coin/tls/fullchain.pem --tls-key /var/lib/sm64coin/tls/privkey.pem   (HTTPS on [::]:16490)
//!   sm64coin-lc ping https://node.example.org
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use sm64coin_lc::client::{READ_TIMEOUT, Transport, UpstreamTransport};
use sm64coin_lc::exec::{self, Upstream};
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::proto::{Request, Response};
use sm64coin_lc::sse::{SseConfig, TlsReloader, serve};
use sm64coin_lc::tipfeed::{TipFeed, TipFeedConfig};

#[derive(Parser)]
#[command(name = "sm64coin-lc", version, about = "SM64Coin light-client service: the chain tip pushed over HTTP")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the tip stream (Server-Sent Events) from the local node's RPC.
    Serve {
        /// The local node's RESTRICTED RPC (sm64coind --rpc-restricted-bind-port 16489; on 127.0.0.1 it needs no opening).
        /// Only allowlisted methods are ever asked of it, and a restricted listener also caps request sizes and keeps sensitive
        /// pool data (Dandelion++ stem transactions) out of the answers.
        #[arg(long, default_value = "http://127.0.0.1:16489")]
        node_rpc: String,
        /// Serve from an unrestricted RPC anyway (closed test networks only).
        #[arg(long)]
        allow_unrestricted_rpc: bool,
        /// Where the stream (GET /lc/tip) listens; repeat for more addresses. Without TLS: plain HTTP, default 127.0.0.1:16490,
        /// the public HTTPS front is the node's reverse proxy (see pow/lc/src/sse.rs) and a loopback address trusts its
        /// X-Forwarded-For. With --tls-cert: HTTPS straight to the public, default [::]:16490 (dual stack; 0.0.0.0:16490 when
        /// IPv6 is unavailable), the client IP is the TCP peer's.
        #[arg(long)]
        sse_bind: Vec<SocketAddr>,
        /// Read the client IP of a subscriber from X-Forwarded-For (only when the listener is reachable only through that proxy;
        /// a loopback --sse-bind does this anyway). Not with TLS.
        #[arg(long)]
        sse_trust_forwarded: bool,
        /// Serve HTTPS: PEM certificate chain (e.g. certbot's fullchain.pem; with --tls-key). The files are looked at every
        /// --tls-check-secs and on SIGHUP, and a changed pair is loaded without a restart (a renewal needs nothing else).
        #[arg(long, requires = "tls_key")]
        tls_cert: Option<std::path::PathBuf>,
        /// PEM private key of --tls-cert (PKCS#8, PKCS#1 or SEC1); readable by the service user.
        #[arg(long, requires = "tls_cert")]
        tls_key: Option<std::path::PathBuf>,
        /// Seconds between looks at the TLS files for a change.
        #[arg(long, default_value_t = 300)]
        tls_check_secs: u64,
    },
    /// Ask a node (https://host or http://host:port) for its tip.
    Ping {
        server: String,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    match Cli::parse().cmd {
        Cmd::Serve { node_rpc, allow_unrestricted_rpc, sse_bind, sse_trust_forwarded, tls_cert, tls_key, tls_check_secs } => {
            let upstream: Arc<dyn Upstream> = Arc::new(HttpUpstream::new(&node_rpc)?);
            while let Err(e) = exec::genesis_hash(&*upstream, Duration::from_secs(5)).await {
                tracing::warn!("waiting for the node at {node_rpc}: {e}");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            match exec::node_restricted(&*upstream, Duration::from_secs(5)).await {
                Some(true) => {}
                _ if allow_unrestricted_rpc => tracing::warn!("serving from the UNRESTRICTED RPC at {node_rpc} (--allow-unrestricted-rpc)"),
                _ => return Err(format!("{node_rpc} is not a restricted RPC: start sm64coind with --rpc-restricted-bind-port 16489 and use --node-rpc http://127.0.0.1:16489").into()),
            }
            let feed = TipFeed::new(upstream, TipFeedConfig::default());
            tokio::spawn(feed.clone().run(|| false));
            let tls = match (&tls_cert, &tls_key) {
                (Some(c), Some(k)) => Some(TlsReloader::load(c, k).map_err(|e| format!("TLS: {e}"))?),
                _ => None,
            };
            if tls.is_some() && sse_trust_forwarded {
                return Err("--sse-trust-forwarded is for a listener behind a proxy; with --tls-cert the listener is reached directly".into());
            }
            let mut binds = sse_bind;
            let default_bind = binds.is_empty();
            if default_bind {
                binds.push(if tls.is_some() { "[::]:16490".parse()? } else { "127.0.0.1:16490".parse()? });
            }
            let mut tasks = Vec::new();
            for bind in binds {
                let mut cfg = SseConfig::behind_local_proxy(bind);
                cfg.trust_forwarded |= sse_trust_forwarded;
                cfg.tls = tls.clone();
                cfg.tls_check = Duration::from_secs(tls_check_secs.max(1));
                let r = match serve(feed.clone(), cfg.clone()).await {
                    Err(e) if default_bind && tls.is_some() => {
                        tracing::warn!("{bind}: {e}; trying 0.0.0.0:16490");
                        cfg.bind = "0.0.0.0:16490".parse()?;
                        serve(feed.clone(), cfg).await
                    }
                    r => r,
                };
                let (addr, task) = r.map_err(|e| format!("{bind}: {e}"))?;
                println!("tip stream (SSE): {}://{addr}/lc/tip", if tls.is_some() { "https" } else { "http" });
                tasks.push(task);
            }
            #[cfg(unix)]
            if let Some(t) = tls.clone() {
                let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
                tokio::spawn(async move {
                    while hup.recv().await.is_some() {
                        match t.reload() {
                            Ok(()) => tracing::info!("TLS certificate reloaded (SIGHUP)"),
                            Err(e) => tracing::warn!("TLS certificate reload failed (keeping the old one): {e}"),
                        }
                    }
                });
            }
            tokio::signal::ctrl_c().await?;
        }
        Cmd::Ping { server } => {
            let t = UpstreamTransport(HttpUpstream::new(&server)?);
            match t.request(&Request::GetTip, READ_TIMEOUT).await? {
                Response::Tip(t) => println!("height {} tip {} difficulty {}", t.height, hex::encode(t.hash), t.cumulative_difficulty),
                other => println!("unexpected: {other:?}"),
            }
        }
    }
    Ok(())
}
