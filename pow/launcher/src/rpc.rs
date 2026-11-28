//! The node's JSON-RPC: the whitelist of methods the website (and the desktop app) may call,
//! and small blocking helpers to call the local node.
use std::io::Read;
use std::time::Duration;

/// Largest answer relayed.
pub const MAX_RESPONSE: u64 = 64 << 20;

/// The daemon calls the website needs: the light-client crate's allowlist (`sm64coin_lc::allowlist`), the one list the
/// service, the launcher and the apps share. The node's RPC is unrestricted (stop_daemon, set_bans, prune_blockchain ...),
/// so the proxy only passes these.
pub use sm64coin_lc::allowlist::{RPC_ALLOWED, path_allowed};

/// May `method` be called through the website / the app? The node's RPC is unrestricted.
pub fn allowed(method: &str) -> bool {
    sm64coin_lc::allowlist::rpc_allowed(method)
}

/// The method of a JSON-RPC request body (one object, no batches).
pub fn method_of(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body).ok().and_then(|v| v.get("method").and_then(|m| m.as_str().map(String::from)))
}

/// POSTs `body` to `<base><path>`; returns (status, content-type, bytes). Upstream HTTP errors are
/// returned as they are; Err only when the node cannot be reached.
pub fn forward(method: &str, base: &str, path: &str, content_type: Option<&str>, accept: Option<&str>, body: &[u8], timeout: Duration) -> Result<(u16, String, Vec<u8>), String> {
    let mut r = ureq::request(method, &format!("{base}{path}")).timeout(timeout);
    if let Some(c) = content_type {
        r = r.set("content-type", c);
    }
    if let Some(a) = accept {
        r = r.set("accept", a);
    }
    let res = if body.is_empty() && method == "GET" { r.call() } else { r.send_bytes(body) };
    let resp = match res {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(format!("upstream unavailable: {e}")),
    };
    let code = resp.status();
    let ct = resp.header("content-type").unwrap_or("application/json").to_string();
    let mut data = Vec::new();
    let _ = resp.into_reader().take(MAX_RESPONSE).read_to_end(&mut data);
    Ok((code, ct, data))
}

/// Calls a whitelisted method of a remote node (`base` = `https://host[:port]`, the public node); returns the whole JSON-RPC answer.
/// Only https (needs the `tls` feature) so the answer cannot be tampered with in transit; the whitelist is the same.
pub fn call_url(base: &str, method: &str, params: serde_json::Value, timeout: Duration) -> Result<serde_json::Value, String> {
    if !allowed(method) {
        return Err(format!("RPC method {method} is not available"));
    }
    if !base.starts_with("https://") || base.contains(['@', '?', '#', ' ']) {
        return Err("a remote node must be an https:// address".into());
    }
    let body = serde_json::json!({"jsonrpc": "2.0", "id": 0, "method": method, "params": params}).to_string();
    // No redirects: the allowed node must answer itself (a 3xx must not lead the call to another host, or to plain http).
    let agent = ureq::AgentBuilder::new().redirects(0).build();
    let res = agent.post(&format!("{}/json_rpc", base.trim_end_matches('/'))).timeout(timeout).set("content-type", "application/json").send_string(&body);
    let resp = match res {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(format!("upstream unavailable: {e}")),
    };
    if resp.status() != 200 {
        return Err(format!("node answered HTTP {}", resp.status()));
    }
    let mut data = Vec::new();
    let _ = resp.into_reader().take(MAX_RESPONSE).read_to_end(&mut data);
    serde_json::from_slice(&data).map_err(|e| format!("bad answer from the node: {e}"))
}

/// Calls a whitelisted method of the node at 127.0.0.1:`port`; returns the whole JSON-RPC answer.
pub fn call(port: u16, method: &str, params: serde_json::Value, timeout: Duration) -> Result<serde_json::Value, String> {
    if !allowed(method) {
        return Err(format!("RPC method {method} is not available"));
    }
    let body = serde_json::json!({"jsonrpc": "2.0", "id": 0, "method": method, "params": params}).to_string();
    let (code, _, data) = forward("POST", &format!("http://127.0.0.1:{port}"), "/json_rpc", Some("application/json"), None, body.as_bytes(), timeout)?;
    if code != 200 {
        return Err(format!("the node answered HTTP {code}"));
    }
    serde_json::from_slice(&data).map_err(|e| format!("bad answer from the node: {e}"))
}

/// The node's `get_info` result, or None if the RPC does not answer.
pub fn get_info(rpc_port: u16) -> Option<serde_json::Value> {
    get_info_timeout(rpc_port, Duration::from_secs(3))
}

/// `get_info` with a caller-chosen timeout: the app's status poll uses a short one so a node still loading its chain cannot
/// hold a UI request for seconds.
pub fn get_info_timeout(rpc_port: u16, timeout: Duration) -> Option<serde_json::Value> {
    let r = ureq::post(&format!("http://127.0.0.1:{rpc_port}/json_rpc"))
        .timeout(timeout)
        .send_string(r#"{"jsonrpc":"2.0","id":0,"method":"get_info"}"#)
        .ok()?;
    let v: serde_json::Value = serde_json::from_reader(r.into_reader()).ok()?;
    v.get("result").cloned()
}

/// The peers the node has finished a handshake with ("a.b.c.d:16480", " in" if they connected to
/// us); None if the RPC doesn't answer (a restricted RPC lists none).
pub fn peers(rpc_port: u16) -> Option<Vec<String>> {
    let r = ureq::post(&format!("http://127.0.0.1:{rpc_port}/json_rpc"))
        .timeout(Duration::from_secs(3))
        .send_string(r#"{"jsonrpc":"2.0","id":0,"method":"get_connections"}"#)
        .ok()?;
    let v: serde_json::Value = serde_json::from_reader(r.into_reader()).ok()?;
    let list = v["result"]["connections"].as_array().cloned().unwrap_or_default();
    Some(
        list.iter()
            .filter(|c| c["state"].as_str().is_some_and(|s| s != "before_handshake"))
            .filter_map(|c| c["address"].as_str().map(|a| format!("{a}{}", if c["incoming"].as_bool() == Some(true) { " in" } else { "" })))
            .collect(),
    )
}
