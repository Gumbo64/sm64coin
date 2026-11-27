//! Blocking-HTTP [`Upstream`] (ureq) run on tokio's blocking pool: talks to a node's RPC, the local
//! one (`http://127.0.0.1:port`) or a remote one.
use std::io::Read;
use std::time::Duration;

use serde_json::Value;

use crate::allowlist;
use crate::exec::{UpError, Upstream};

/// Largest answer read from a node.
pub const MAX_RESPONSE: u64 = 64 << 20;

#[derive(Clone, Debug)]
pub struct HttpUpstream {
    base: String,
    agent: ureq::Agent,
    /// Where the tip stream is when it is not `<base>/lc/tip` (the light-client service on its own HTTPS port).
    tip_url: Option<String>,
}

impl HttpUpstream {
    /// `base` = `http://host:port` or `https://host[:port]` (https needs the `tls` feature). No redirects are followed.
    pub fn new(base: &str) -> Result<Self, UpError> {
        if !(base.starts_with("http://") || base.starts_with("https://")) || base.contains(['@', '?', '#', ' ']) {
            return Err(UpError::NotAllowed("a node must be an http(s):// address".into()));
        }
        Ok(Self { base: base.trim_end_matches('/').to_string(), agent: ureq::AgentBuilder::new().redirects(0).build(), tip_url: None })
    }
    /// The tip stream at `url` (a full URL) instead of `<base>/lc/tip`.
    pub fn with_tip_url(mut self, url: Option<String>) -> Self {
        self.tip_url = url;
        self
    }
    pub fn base(&self) -> &str {
        &self.base
    }
    fn post_blocking(&self, path: &str, content_type: &str, body: Vec<u8>, timeout: Duration) -> Result<Vec<u8>, UpError> {
        let res = self.agent.post(&format!("{}{path}", self.base)).timeout(timeout).set("content-type", content_type).send_bytes(&body);
        let resp = match res {
            Ok(r) => r,
            Err(ureq::Error::Status(c, _)) => return Err(UpError::Status(c)),
            Err(e) => return Err(UpError::Unreachable(e.to_string())),
        };
        let mut data = Vec::new();
        resp.into_reader().take(MAX_RESPONSE).read_to_end(&mut data).map_err(|e| UpError::Bad(e.to_string()))?;
        Ok(data)
    }
}

#[async_trait::async_trait]
impl Upstream for HttpUpstream {
    async fn json_rpc(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, UpError> {
        if !allowlist::rpc_allowed(method) {
            return Err(UpError::NotAllowed(format!("RPC method {method} is not available")));
        }
        let body = serde_json::json!({"jsonrpc": "2.0", "id": 0, "method": method, "params": params}).to_string().into_bytes();
        let me = self.clone();
        let data = tokio::task::spawn_blocking(move || me.post_blocking("/json_rpc", "application/json", body, timeout)).await.map_err(|e| UpError::Unreachable(e.to_string()))??;
        serde_json::from_slice(&data).map_err(|e| UpError::Bad(e.to_string()))
    }
    async fn tip_stream(&self) -> Option<Result<Box<dyn crate::client::TipStream>, crate::client::TransportError>> {
        Some(self.open_sse().await)
    }
    async fn post(&self, path: &str, content_type: &str, body: Vec<u8>, timeout: Duration) -> Result<Vec<u8>, UpError> {
        if !allowlist::path_allowed(path) {
            return Err(UpError::NotAllowed(format!("{path} is not available")));
        }
        let me = self.clone();
        let (path, ct) = (path.to_string(), content_type.to_string());
        tokio::task::spawn_blocking(move || me.post_blocking(&path, &ct, body, timeout)).await.map_err(|e| UpError::Unreachable(e.to_string()))?
    }
}

/// Where an HTTP node serves its tip stream (Server-Sent Events).
pub const SSE_PATH: &str = "/lc/tip";

/// A tip subscription over Server-Sent Events: a blocking reader thread parses the events and hands the tips over a channel.
struct SseStream {
    rx: tokio::sync::mpsc::UnboundedReceiver<crate::proto::TipInfo>,
}

#[async_trait::async_trait]
impl crate::client::TipStream for SseStream {
    async fn next(&mut self) -> Result<Option<crate::proto::TipInfo>, crate::client::TransportError> {
        Ok(self.rx.recv().await)
    }
}

impl HttpUpstream {
    /// GET `/lc/tip` (`Accept: text/event-stream`; at the node's advertised tip URL when it has one): `Unsupported` when the node
    /// has no such endpoint (404/405/501; for an advertised tip URL any failure, e.g. a closed port or an HTTP error: that
    /// must not count against the node's RPC), else a stream that ends when the connection does (the caller reconnects). The
    /// server sends a `ka` event every 30 s, so 95 s of silence is a dead connection.
    async fn open_sse(&self) -> Result<Box<dyn crate::client::TipStream>, crate::client::TransportError> {
        use crate::client::TransportError;
        let advertised = self.tip_url.is_some();
        let url = self.tip_url.clone().unwrap_or_else(|| format!("{}{}", self.base, SSE_PATH));
        let agent = ureq::AgentBuilder::new().redirects(0).timeout_connect(Duration::from_secs(10)).timeout_read(Duration::from_secs(95)).build();
        let resp = tokio::task::spawn_blocking(move || agent.get(&url).set("Accept", "text/event-stream").call())
            .await
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        let resp = match resp {
            Ok(r) => r,
            // an advertised tip URL is a separate service: whatever goes wrong there (closed port, TLS, a proxy's 502) is no
            // reason to strike the node's RPC, so it only means "no push here" and the caller polls
            Err(_) if advertised => return Err(TransportError::Unsupported),
            Err(ureq::Error::Status(404 | 405 | 501, _)) => return Err(TransportError::Unsupported),
            Err(ureq::Error::Status(c, _)) => return Err(TransportError::Protocol(format!("HTTP {c}"))),
            Err(e) => return Err(TransportError::Connect(e.to_string())),
        };
        if !resp.content_type().starts_with("text/event-stream") {
            return Err(TransportError::Unsupported);
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::task::spawn_blocking(move || {
            use std::io::BufRead;
            let mut lines = std::io::BufReader::new(resp.into_reader()).lines();
            let (mut event, mut data) = (String::new(), String::new());
            while let Some(Ok(line)) = lines.next() {
                if line.is_empty() {
                    // end of an event: the default (unnamed) ones are tips; `ka` is the keepalive
                    if event.is_empty() && !data.is_empty() {
                        if let Some(t) = serde_json::from_str::<Value>(&data).ok().and_then(|j| crate::exec::tip_from_record(&j)) {
                            if tx.send(t).is_err() {
                                return;
                            }
                        }
                    }
                    if tx.is_closed() {
                        return;
                    }
                    event.clear();
                    data.clear();
                } else if let Some(v) = line.strip_prefix("event:") {
                    event = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("data:") {
                    data.push_str(v.trim_start());
                }
            }
        });
        Ok(Box::new(SseStream { rx }))
    }
}
