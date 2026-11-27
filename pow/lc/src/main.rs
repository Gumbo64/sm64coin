//! `sm64coin-lc`: the light-client service next to a node: the chain tip pushed over HTTP (Server-Sent Events, `GET /lc/tip`),
//! with a tiny `ping` client. The public HTTPS address is the node's reverse proxy (see `sse.rs`).
//!
//!   sm64coin-lc serve --node-rpc http://127.0.0.1:16489 --sse-bind 127.0.0.1:16490
//!   sm64coin-lc ping https://node.example.org
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use sm64coin_lc::client::{READ_TIMEOUT, Transport, UpstreamTransport};
use sm64coin_lc::exec::{self, Upstream};
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::proto::{Request, Response};
use sm64coin_lc::sse::{SseConfig, serve};
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
        /// The plain-HTTP address of the stream (GET /lc/tip); the public HTTPS front is the node's reverse proxy (see
        /// pow/lc/src/sse.rs). A loopback address trusts the proxy's X-Forwarded-For.
        #[arg(long, default_value = "127.0.0.1:16490")]
        sse_bind: SocketAddr,
        /// Read the client IP of a subscriber from X-Forwarded-For (only when the listener is reachable only through that proxy;
        /// a loopback --sse-bind does this anyway).
        #[arg(long)]
        sse_trust_forwarded: bool,
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
        Cmd::Serve { node_rpc, allow_unrestricted_rpc, sse_bind, sse_trust_forwarded } => {
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
            let mut cfg = SseConfig::behind_local_proxy(sse_bind);
            cfg.trust_forwarded |= sse_trust_forwarded;
            let (addr, _task) = serve(feed, cfg).await?;
            println!("tip stream (SSE): http://{addr}/lc/tip");
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
