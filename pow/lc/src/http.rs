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
}

impl HttpUpstream {
    /// `base` = `http://host:port` or `https://host[:port]` (https needs the `tls` feature). No redirects are followed.
    pub fn new(base: &str) -> Result<Self, UpError> {
        if !(base.starts_with("http://") || base.starts_with("https://")) || base.contains(['@', '?', '#', ' ']) {
            return Err(UpError::NotAllowed("a node must be an http(s):// address".into()));
        }
        Ok(Self { base: base.trim_end_matches('/').to_string(), agent: ureq::AgentBuilder::new().redirects(0).build() })
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
    async fn post(&self, path: &str, content_type: &str, body: Vec<u8>, timeout: Duration) -> Result<Vec<u8>, UpError> {
        if !allowlist::path_allowed(path) {
            return Err(UpError::NotAllowed(format!("{path} is not available")));
        }
        let me = self.clone();
        let (path, ct) = (path.to_string(), content_type.to_string());
        tokio::task::spawn_blocking(move || me.post_blocking(&path, &ct, body, timeout)).await.map_err(|e| UpError::Unreachable(e.to_string()))?
    }
}
