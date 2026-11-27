//! The loopback daemon of the wallet (and of the Android native miner): `http://127.0.0.1:<random port>`.
//!
//! The wallet engine (monero-ts `wallet2` in the webview, the wallet-rpc sidecar) and the native miner's `Campaign` each take one
//! fixed daemon address. Pointing them at this proxy gives them failover: every call goes through `lcnet::Net` (the app's
//! light-client network), which picks a node, moves on when it fails and fans submissions out to three nodes. The proxy
//! forwards only the wallet's daemon calls (the RPC allowlist and the `.bin` / `send_raw_transaction` paths of
//! `sm64coin_lc::allowlist`), refuses any Host that is not itself (DNS rebinding) and any Origin that is not the app's own pages.
use crate::lcnet::Net;
use serde_json::{Value, json};
use sm64coin_lc::allowlist;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The webview's origins (Tauri: tauri://localhost on Linux/macOS/Android, http(s)://tauri.localhost on Windows).
pub const APP_ORIGINS: &[&str] = &["tauri://localhost", "http://tauri.localhost", "https://tauri.localhost"];
const MAX_BODY: usize = 8 << 20;
const MAX_IN_FLIGHT: usize = 32;

pub struct WalletProxy {
    server: Arc<tiny_http::Server>,
    pub port: u16,
}

impl WalletProxy {
    /// `http://127.0.0.1:port`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    /// `127.0.0.1:port` (the wallet-rpc sidecar's `--daemon-address` form).
    pub fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

impl Drop for WalletProxy {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

/// Starts the proxy on a random loopback port. `extra_origins`: more allowed Origins (tests).
pub fn start(net: Arc<Net>, extra_origins: Vec<String>) -> Result<Arc<WalletProxy>, String> {
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").map_err(|e| e.to_string())?);
    let port = server.server_addr().to_ip().ok_or("no loopback address")?.port();
    net.set_proxy(&format!("http://127.0.0.1:{port}"));
    let origins: Arc<Vec<String>> = Arc::new(APP_ORIGINS.iter().map(|s| s.to_string()).chain(extra_origins).collect());
    let s2 = server.clone();
    let in_flight = Arc::new(AtomicUsize::new(0));
    std::thread::Builder::new()
        .name("wallet-proxy".into())
        .spawn(move || {
            for req in s2.incoming_requests() {
                if in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    let _ = req.respond(tiny_http::Response::from_string("busy").with_status_code(503));
                    continue;
                }
                let (net, origins, n) = (net.clone(), origins.clone(), in_flight.clone());
                std::thread::spawn(move || {
                    handle(req, &net, port, &origins);
                    n.fetch_sub(1, Ordering::SeqCst);
                });
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(WalletProxy { server, port }))
}

fn header(k: &str, v: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn hv(req: &tiny_http::Request, name: &str) -> Option<String> {
    req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string())
}

fn reply(req: tiny_http::Request, code: u16, ct: &str, body: Vec<u8>, origin: Option<&str>) {
    let mut r = tiny_http::Response::from_data(body).with_status_code(code).with_header(header("Content-Type", ct));
    if let Some(o) = origin {
        r = r.with_header(header("Access-Control-Allow-Origin", o)).with_header(header("Vary", "Origin"));
    }
    let _ = req.respond(r);
}

fn handle(mut req: tiny_http::Request, net: &Net, port: u16, origins: &[String]) {
    let host = hv(&req, "host").unwrap_or_default().to_ascii_lowercase();
    // wallet2 sends `Host: 127.0.0.1` without the port; any other name (DNS rebinding) is refused.
    if ![format!("127.0.0.1:{port}"), format!("localhost:{port}"), "127.0.0.1".into(), "localhost".into()].contains(&host) {
        return reply(req, 403, "text/plain", b"wrong Host".to_vec(), None);
    }
    let origin = hv(&req, "origin");
    if let Some(o) = &origin {
        if !origins.iter().any(|a| a.eq_ignore_ascii_case(o.trim_end_matches('/'))) {
            return reply(req, 403, "text/plain", b"foreign Origin".to_vec(), None);
        }
    }
    let o = origin.as_deref();
    if *req.method() == tiny_http::Method::Options {
        let mut r = tiny_http::Response::empty(204)
            .with_header(header("Access-Control-Allow-Methods", "POST, GET, OPTIONS"))
            .with_header(header("Access-Control-Allow-Headers", &hv(&req, "access-control-request-headers").unwrap_or_else(|| "content-type".into())))
            .with_header(header("Access-Control-Max-Age", "600"));
        if let Some(o) = o {
            r = r.with_header(header("Access-Control-Allow-Origin", o)).with_header(header("Vary", "Origin"));
        }
        return void(req.respond(r));
    }
    let path = req.url().split('?').next().unwrap_or("").to_string();
    if req.body_length().is_some_and(|n| n > MAX_BODY) {
        return reply(req, 413, "text/plain", b"request too large".to_vec(), o);
    }
    let mut body = Vec::new();
    let _ = req.as_reader().take(MAX_BODY as u64 + 1).read_to_end(&mut body);
    if body.len() > MAX_BODY {
        return reply(req, 413, "text/plain", b"request too large".to_vec(), o);
    }
    if path == "/json_rpc" {
        let v: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return reply(req, 400, "text/plain", b"not JSON".to_vec(), o),
        };
        let Some(method) = v.get("method").and_then(|m| m.as_str()) else { return reply(req, 400, "text/plain", b"one JSON-RPC object with a method".to_vec(), o) };
        if !allowlist::rpc_allowed(method) || allowlist::pinned_only(method) {
            eprintln!("wallet proxy: refused json_rpc {method}");
            return reply(req, 403, "text/plain", b"this RPC method is not available".to_vec(), o);
        }
        let params = v.get("params").cloned().unwrap_or_else(|| json!({}));
        let id = v.get("id").cloned().unwrap_or(json!(0));
        return match tauri::async_runtime::block_on(net.rpc(method, params)) {
            Ok(mut a) => {
                if let Some(m) = a.as_object_mut() {
                    m.insert("id".into(), id);
                }
                reply(req, 200, "application/json", a.to_string().into_bytes(), o)
            }
            Err(e) => reply(req, 502, "text/plain", e.into_bytes(), o),
        };
    }
    if path == "/getheight" {
        // wallet2 asks for the chain height this way first; not in the shared path allowlist, answered from the tip.
        return match tauri::async_runtime::block_on(net.height()) {
            Ok(h) => reply(req, 200, "application/json", json!({"height": h, "status": "OK", "untrusted": false}).to_string().into_bytes(), o),
            Err(e) => reply(req, 502, "text/plain", e.into_bytes(), o),
        };
    }
    if allowlist::path_allowed(&path) && path != "/get_public_nodes" {
        return match tauri::async_runtime::block_on(net.wallet_post(&path, body)) {
            Ok((ct, data)) => reply(req, 200, ct, data, o),
            Err(e) => reply(req, 502, "text/plain", e.into_bytes(), o),
        };
    }
    eprintln!("wallet proxy: refused {} {path}", req.method());
    reply(req, 403, "text/plain", b"this daemon call is not available".to_vec(), o)
}

fn void<T, E>(_: Result<T, E>) {}
