//! Request -> node RPC. The single place that knows how a protocol [`Request`] maps onto the node's
//! JSON-RPC / `.bin` calls and how the answer is shaped into a [`Response`]. Used by the server
//! (against the local node) and by [`crate::client::UpstreamTransport`] (against a remote HTTP node),
//! so both transports give identical answers.
use std::time::Duration;

use serde_json::{Value, json};

use crate::allowlist;
use crate::proto::*;

/// `Send` on native targets; nothing in the browser (single-threaded futures).
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send> MaybeSend for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSend for T {}

#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSync: Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync> MaybeSync for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSync {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSync for T {}

#[derive(Debug, thiserror::Error)]
pub enum UpError {
    #[error("node unreachable: {0}")]
    Unreachable(String),
    #[error("node answered HTTP {0}")]
    Status(u16),
    #[error("bad answer from the node: {0}")]
    Bad(String),
    #[error("not allowed: {0}")]
    NotAllowed(String),
}

/// How to talk to a node's HTTP RPC. Native: [`crate::http::HttpUpstream`]; the browser plugs in `fetch`.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait Upstream: MaybeSend + MaybeSync {
    /// POST `/json_rpc`; returns the whole JSON-RPC answer. Implementations enforce [`allowlist::rpc_allowed`].
    async fn json_rpc(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, UpError>;
    /// POST `path` (an [`allowlist::PATH_ALLOWED`] path); returns the answer body.
    async fn post(&self, path: &str, content_type: &str, body: Vec<u8>, timeout: Duration) -> Result<Vec<u8>, UpError>;
}

fn bin_path(c: WalletBin) -> &'static str {
    match c {
        WalletBin::GetBlocks => "/getblocks.bin",
        WalletBin::GetOuts => "/get_outs.bin",
        WalletBin::GetHashes => "/gethashes.bin",
        WalletBin::GetPoolHashes => "/get_transaction_pool_hashes.bin",
    }
}

/// Range / size checks that hold for every transport.
pub fn validate(req: &Request) -> Result<(), ErrorKind> {
    match req {
        Request::GetHeaders { count, .. } if *count == 0 || *count > MAX_HEADERS => Err(ErrorKind::Malformed),
        Request::GetBlocks { count, .. } if *count == 0 || *count > MAX_BLOCKS => Err(ErrorKind::Malformed),
        Request::GetNodes { max } if *max == 0 || *max > MAX_NODES => Err(ErrorKind::Malformed),
        Request::GetCycle { hash } if !(hash.is_empty() || (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))) => Err(ErrorKind::Malformed),
        Request::GetTemplate { address, reserve } if address.len() > 200 || address.is_empty() || *reserve > 255 => Err(ErrorKind::Malformed),
        Request::GetOutDistribution { amounts, .. } if amounts.len() > 64 => Err(ErrorKind::Malformed),
        _ => Ok(()),
    }
}

fn is_local(req: &Request) -> bool {
    matches!(req, Request::Hello { .. } | Request::GetNodes { .. } | Request::Probe { .. })
}

fn upstream_err(e: UpError) -> Response {
    Response::Error(ErrorKind::Upstream(e.to_string()))
}

async fn rpc(up: &dyn Upstream, method: &str, params: Value, t: Duration) -> Result<Value, Response> {
    debug_assert!(allowlist::rpc_allowed(method));
    let v = up.json_rpc(method, params, t).await.map_err(upstream_err)?;
    if let Some(e) = v.get("error") {
        let msg = e.get("message").and_then(|m| m.as_str()).unwrap_or("error").to_string();
        return Err(Response::Error(ErrorKind::Upstream(msg)));
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    hex::decode(s).ok()?.try_into().ok()
}

/// Parses a `get_info` result into a [`Tip`].
pub fn tip_from_info(info: &Value) -> Option<Tip> {
    let height = info.get("height")?.as_u64()?;
    let hash = hex32(info.get("top_block_hash")?.as_str()?)?;
    let cd = match info.get("wide_cumulative_difficulty").and_then(|v| v.as_str()) {
        Some(s) => u128::from_str_radix(s.trim_start_matches("0x"), 16).ok()?,
        None => info.get("cumulative_difficulty")?.as_u64()? as u128,
    };
    // `height` is the count of blocks; the tip's height is one less.
    Some(Tip { height: height.saturating_sub(1), hash, cumulative_difficulty: cd })
}

/// Answers a request from the node behind `up`. `Hello`, `GetNodes` and `Probe` are not node calls
/// (the server answers them itself) and give `Unsupported` here.
pub async fn run(up: &dyn Upstream, req: &Request, timeout: Duration) -> Response {
    if let Err(e) = validate(req) {
        return Response::Error(e);
    }
    if is_local(req) {
        return Response::Error(ErrorKind::Unsupported);
    }
    match run_inner(up, req, timeout).await {
        Ok(r) | Err(r) => r,
    }
}

async fn run_inner(up: &dyn Upstream, req: &Request, t: Duration) -> Result<Response, Response> {
    Ok(match req {
        Request::GetTip => {
            let info = rpc(up, "get_info", json!({}), t).await?;
            match tip_from_info(&info) {
                Some(tip) => Response::Tip(tip),
                None => Response::Error(ErrorKind::Upstream("unreadable get_info".into())),
            }
        }
        Request::GetHeaders { from, count } => {
            let r = rpc(up, "get_block_headers_range", json!({"start_height": from, "end_height": from + *count as u64 - 1}), t).await?;
            Response::Json(r.to_string())
        }
        Request::GetBlocks { from, count, max_bytes } => {
            let mut out = Vec::new();
            let mut bytes = 0usize;
            for h in *from..*from + *count as u64 {
                let r = match rpc(up, "get_block", json!({"height": h}), t).await {
                    Ok(r) => r,
                    Err(e) if out.is_empty() => return Err(e),
                    Err(_) => break,
                };
                let s = r.to_string();
                if !out.is_empty() && bytes + s.len() > *max_bytes as usize {
                    break;
                }
                bytes += s.len();
                out.push(s);
                if bytes > MAX_FRAME / 2 {
                    break;
                }
            }
            Response::Blocks(out)
        }
        Request::GetCycle { hash } => {
            let params = if hash.is_empty() { json!({}) } else { json!({"hash": hash}) };
            Response::Json(rpc(up, "get_sm64_cycle", params, t).await?.to_string())
        }
        Request::GetTemplate { address, reserve } => {
            Response::Json(rpc(up, "get_block_template", json!({"wallet_address": address, "reserve_size": reserve}), t).await?.to_string())
        }
        Request::SubmitBlock { blob } => {
            let v = up.json_rpc("submit_block", json!([hex::encode(blob)]), t).await.map_err(upstream_err)?;
            match v.get("error") {
                Some(e) => Response::Rejected { reason: e.get("message").and_then(|m| m.as_str()).unwrap_or("rejected").to_string() },
                None => match v["result"]["status"].as_str() {
                    Some("OK") => Response::Accepted,
                    other => Response::Rejected { reason: other.unwrap_or("rejected").to_string() },
                },
            }
        }
        Request::SubmitProof { blob } => {
            let r = rpc(up, "submit_sm64_proof", json!({"blob": hex::encode(blob)}), t).await?;
            match r["result"].as_i64() {
                Some(1) => Response::Accepted,
                _ => Response::Rejected { reason: r["reason"].as_str().unwrap_or("not accepted").to_string() },
            }
        }
        Request::Wallet { call, body } => {
            let path = bin_path(*call);
            debug_assert!(allowlist::path_allowed(path));
            Response::Bin(up.post(path, "application/octet-stream", body.clone(), t).await.map_err(upstream_err)?)
        }
        Request::GetOutDistribution { amounts, from_height, to_height } => {
            let r = rpc(up, "get_output_distribution", json!({"amounts": amounts, "from_height": from_height, "to_height": to_height, "cumulative": false}), t).await?;
            Response::Json(r.to_string())
        }
        Request::GetFee => Response::Json(rpc(up, "get_fee_estimate", json!({}), t).await?.to_string()),
        Request::SendTx { blob } => {
            let body = json!({"tx_as_hex": hex::encode(blob), "do_not_relay": false}).to_string().into_bytes();
            let raw = up.post("/send_raw_transaction", "application/json", body, t).await.map_err(upstream_err)?;
            let v: Value = serde_json::from_slice(&raw).map_err(|e| upstream_err(UpError::Bad(e.to_string())))?;
            match v["status"].as_str() {
                Some("OK") => Response::Accepted,
                other => Response::Rejected { reason: v["reason"].as_str().filter(|r| !r.is_empty()).or(other).unwrap_or("rejected").to_string() },
            }
        }
        Request::Hello { .. } | Request::GetNodes { .. } | Request::Probe { .. } => Response::Error(ErrorKind::Unsupported),
    })
}

/// The chain's genesis block hash (`get_block_header_by_height 0`): the `network` of `Hello`.
pub async fn genesis_hash(up: &dyn Upstream, timeout: Duration) -> Result<[u8; 32], String> {
    let r = rpc(up, "get_block_header_by_height", json!({"height": 0}), timeout).await.map_err(|e| format!("{e:?}"))?;
    r.get("block_header").and_then(|h| h.get("hash")).and_then(|h| h.as_str()).and_then(hex32).ok_or_else(|| "no genesis hash in the answer".to_string())
}
