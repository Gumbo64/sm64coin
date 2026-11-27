//! `fetch` as the [`Upstream`] of an HTTP(S) node (the browser has no other way to talk HTTP).
use std::time::Duration;

use serde_json::Value;
use sm64coin_lc::allowlist;
use sm64coin_lc::exec::{UpError, Upstream};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

pub struct FetchUpstream {
    base: String,
}

impl FetchUpstream {
    pub fn new(base: &str) -> Self {
        Self { base: base.trim_end_matches('/').to_string() }
    }
}

/// Cancels the request when dropped (a timeout drops the future).
struct AbortOnDrop(web_sys::AbortController);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

const MAX_BODY: usize = 64 << 20;

fn js_err(e: JsValue) -> UpError {
    UpError::Unreachable(e.as_string().or_else(|| js_sys::Reflect::get(&e, &"message".into()).ok().and_then(|m| m.as_string())).unwrap_or_else(|| "fetch failed".into()))
}

async fn post(url: &str, content_type: &str, body: JsValue, timeout: Duration) -> Result<Vec<u8>, UpError> {
    let work = async {
        let ctl = web_sys::AbortController::new().map_err(js_err)?;
        let _guard = AbortOnDrop(ctl.clone());
        let init = js_sys::Object::new();
        let set = |k: &str, v: JsValue| js_sys::Reflect::set(&init, &k.into(), &v).map(|_| ()).map_err(js_err);
        set("method", "POST".into())?;
        let headers = js_sys::Object::new();
        js_sys::Reflect::set(&headers, &"Content-Type".into(), &content_type.into()).map_err(js_err)?;
        set("headers", headers.into())?;
        set("body", body)?;
        set("signal", ctl.signal().into())?;
        set("credentials", "omit".into())?;
        set("redirect", "error".into())?;
        let global = js_sys::global();
        let fetch: js_sys::Function = js_sys::Reflect::get(&global, &"fetch".into()).map_err(js_err)?.dyn_into().map_err(|_| UpError::Unreachable("no fetch".into()))?;
        let promise: js_sys::Promise = fetch.call2(&global, &url.into(), &init).map_err(js_err)?.dyn_into().map_err(|_| UpError::Unreachable("fetch gave no promise".into()))?;
        let resp = JsFuture::from(promise).await.map_err(js_err)?;
        let ok = js_sys::Reflect::get(&resp, &"ok".into()).map_err(js_err)?.as_bool().unwrap_or(false);
        if !ok {
            let status = js_sys::Reflect::get(&resp, &"status".into()).map_err(js_err)?.as_f64().unwrap_or(0.0) as u16;
            return Err(UpError::Status(status));
        }
        // a node that announces a huge answer is refused before the browser buffers it
        let headers = js_sys::Reflect::get(&resp, &"headers".into()).map_err(js_err)?;
        if let Ok(get) = js_sys::Reflect::get(&headers, &"get".into()).and_then(|g| g.dyn_into::<js_sys::Function>().map_err(JsValue::from)) {
            let len = get.call1(&headers, &"content-length".into()).ok().and_then(|v| v.as_string()).and_then(|s| s.trim().parse::<usize>().ok());
            if len.is_some_and(|n| n > MAX_BODY) {
                return Err(UpError::Bad("response too large".into()));
            }
        }
        let ab = js_sys::Reflect::get(&resp, &"arrayBuffer".into()).map_err(js_err)?.dyn_into::<js_sys::Function>().map_err(|_| UpError::Bad("no arrayBuffer".into()))?;
        let buf = JsFuture::from(ab.call0(&resp).map_err(js_err)?.dyn_into::<js_sys::Promise>().map_err(|_| UpError::Bad("no promise".into()))?).await.map_err(js_err)?;
        let bytes = js_sys::Uint8Array::new(&buf);
        if bytes.length() as usize > MAX_BODY {
            return Err(UpError::Bad("response too large".into()));
        }
        Ok(bytes.to_vec())
    };
    // the request, or the timer, whichever finishes first (the loser is dropped: a dropped request aborts its fetch)
    let timer = sleep(timeout);
    futures_util::pin_mut!(work, timer);
    match futures_util::future::select(work, timer).await {
        futures_util::future::Either::Left((r, _)) => r,
        futures_util::future::Either::Right(_) => Err(UpError::Unreachable("timed out".into())),
    }
}

/// `setTimeout` as a future.
async fn sleep(d: Duration) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let global = js_sys::global();
        if let Ok(set) = js_sys::Reflect::get(&global, &"setTimeout".into()).and_then(|f| f.dyn_into::<js_sys::Function>().map_err(JsValue::from)) {
            let _ = set.call2(&global, &resolve, &JsValue::from_f64(d.as_millis() as f64));
        }
    });
    let _ = JsFuture::from(promise).await;
}

#[async_trait::async_trait(?Send)]
impl Upstream for FetchUpstream {
    async fn json_rpc(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, UpError> {
        if !allowlist::rpc_allowed(method) {
            return Err(UpError::NotAllowed(method.to_string()));
        }
        // text/plain keeps the POST a CORS "simple request": no preflight before every call (monerod reads the body whatever the type)
        let body = serde_json::json!({"jsonrpc": "2.0", "id": 0, "method": method, "params": params}).to_string();
        let raw = post(&format!("{}/json_rpc", self.base), "text/plain;charset=UTF-8", body.into(), timeout).await?;
        serde_json::from_slice(&raw).map_err(|e| UpError::Bad(e.to_string()))
    }

    async fn post(&self, path: &str, content_type: &str, body: Vec<u8>, timeout: Duration) -> Result<Vec<u8>, UpError> {
        if !allowlist::path_allowed(path) {
            return Err(UpError::NotAllowed(path.to_string()));
        }
        let arr = js_sys::Uint8Array::from(body.as_slice());
        post(&format!("{}{}", self.base, path), content_type, arr.into(), timeout).await
    }
}
