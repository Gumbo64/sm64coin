//! `--lc`: the light-client service (sm64coin-lc: the chain tip pushed over HTTP as Server-Sent Events at `GET /lc/tip`) run in
//! this process next to the node the launcher started. The same code as the standalone `sm64coin-lc serve`
//! (pow/lc/src/main.rs), which is what a node run by hand or by systemd uses (packaging/sm64coin-lc.service).
//! Plain HTTP on a local port; the public HTTPS address is the node's reverse proxy (see `sm64coin_lc::sse`).
//! Runs on its own thread with its own tokio runtime; nothing here touches the node except its restricted-safe RPC on
//! 127.0.0.1 (only allowlisted methods are ever asked, see `sm64coin_lc::allowlist`).
use sm64coin_lc::exec::{self, Upstream};
use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::sse::{SseConfig, serve};
use sm64coin_lc::tipfeed::{TipFeed, TipFeedConfig};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct LcConfig {
    /// The local node's RESTRICTED RPC port (127.0.0.1; `--rpc-restricted-bind-port`). The service refuses to start on an
    /// unrestricted one unless `allow_unrestricted` (tests): monerod's unrestricted RPC has no request caps and returns
    /// Dandelion++ stem transactions in the pool calls.
    pub node_rpc_port: u16,
    pub allow_unrestricted: bool,
    /// Where the tip stream (`GET /lc/tip`) listens, behind the node's HTTPS reverse proxy. A loopback address trusts the
    /// proxy's `X-Forwarded-For`.
    pub sse_bind: SocketAddr,
}

impl LcConfig {
    pub fn new(node_rpc_port: u16) -> Self {
        LcConfig { node_rpc_port, allow_unrestricted: false, sse_bind: "127.0.0.1:16490".parse().unwrap() }
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
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match exec::genesis_hash(&*upstream, Duration::from_secs(5)).await {
            Ok(_) => break,
            Err(_) => tokio::time::sleep(Duration::from_secs(3)).await,
        }
    }
    if !cfg.allow_unrestricted && exec::node_restricted(&*upstream, Duration::from_secs(5)).await != Some(true) {
        return Err(format!("the node RPC on port {} is not restricted (start sm64coind with --rpc-restricted-bind-port and point the service at that port)", cfg.node_rpc_port).into());
    }
    let feed = TipFeed::new(upstream, TipFeedConfig::default());
    let stop2 = stop.clone();
    let poller = tokio::spawn(feed.clone().run(move || stop2.load(Ordering::SeqCst)));
    let (addr, task) = serve(feed, SseConfig::behind_local_proxy(cfg.sse_bind)).await?;
    println!("Light-client service: tip stream (SSE) at http://{addr}/lc/tip");
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    task.abort();
    poller.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// The service starts against a (fake) node RPC, streams the tip over SSE and stops.
    #[test]
    fn starts_next_to_a_node_streams_the_tip_and_stops() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let hash = "ab".repeat(32);
        std::thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let mut b = String::new();
                let _ = req.as_reader().read_to_string(&mut b);
                let result = if b.contains("get_info") {
                    serde_json::json!({"height": 5, "top_block_hash": hash, "cumulative_difficulty": 7, "difficulty": 100, "status": "OK"})
                } else {
                    serde_json::json!({"block_header": {"hash": hash, "height": 4, "timestamp": 1_700_000_000u64}, "status": "OK"})
                };
                let body = serde_json::json!({"jsonrpc": "2.0", "id": 0, "result": result}).to_string();
                let _ = req.respond(tiny_http::Response::from_string(body));
            }
        });
        let sse_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut c = LcConfig::new(port);
        c.allow_unrestricted = true; // the fake node says nothing about `restricted`
        c.sse_bind = format!("127.0.0.1:{sse_port}").parse().unwrap();
        let h = start(c).unwrap();
        let t = std::time::Instant::now();
        let mut got = String::new();
        while !got.contains("\"height\":4") {
            assert!(t.elapsed() < Duration::from_secs(20), "no tip from the service: {got:?}");
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", sse_port)) {
                s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let _ = s.write_all(b"GET /lc/tip HTTP/1.1\r\nHost: x\r\nAccept: text/event-stream\r\n\r\n");
                let mut buf = [0u8; 2048];
                if let Ok(n) = s.read(&mut buf) {
                    got.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
                // the first read may hold only the headers: read once more for the first event
                if let Ok(n) = s.read(&mut buf) {
                    got.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        h.stop();
        std::thread::sleep(Duration::from_millis(1500));
    }
}
