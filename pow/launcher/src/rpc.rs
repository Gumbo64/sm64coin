//! The node's JSON-RPC: the whitelist of methods the website (and the desktop app) may call,
//! and small blocking helpers to call the local node.
use std::io::Read;
use std::time::Duration;

/// Largest answer relayed.
pub const MAX_RESPONSE: u64 = 64 << 20;

/// The daemon calls the website needs. The node's RPC is unrestricted (stop_daemon, set_bans,
/// prune_blockchain ...), so the proxy only passes these.
pub const RPC_ALLOWED: &[&str] = &[
    "get_info", "get_block_count", "get_block", "get_block_header_by_hash", "get_block_header_by_height",
    "get_block_headers_range", "get_block_template", "submit_block",
    // proof-of-play (the miner's parent state and cycle) and the node's leaderboard
    "get_sm64_cycle", "get_sm64_state", "sm64_board", "sm64_board_join",
    // read-only calls a wallet makes (restricted-RPC safe)
    "get_version", "get_fee_estimate", "get_last_block_header", "get_output_distribution", "hard_fork_info",
];

/// May `method` be called through the website / the app? The node's RPC is unrestricted.
pub fn allowed(method: &str) -> bool {
    RPC_ALLOWED.contains(&method)
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
    let (code, _, data) = forward("POST", base.trim_end_matches('/'), "/json_rpc", Some("application/json"), None, body.as_bytes(), timeout)?;
    if code != 200 {
        return Err(format!("node answered HTTP {code}"));
    }
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
    let r = ureq::post(&format!("http://127.0.0.1:{rpc_port}/json_rpc"))
        .timeout(Duration::from_secs(3))
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
