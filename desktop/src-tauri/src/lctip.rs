//! The chain tip for the page: Rust follows it once (lcnet.rs `TipHub`: the light-client network's SubscribeTip push or a slow
//! poll, and on desktop the app's own node polled locally) and tells the webview with a `tip` event, so the page's tip store
//! (web/src/lib/tip.js, through `platform.tip`) needs no `get_info` of its own. The event record has the shape lc-wasm's
//! `watch_tip` gives the browser: `{node, height, hash, timestamp, cumulative_difficulty, difficulty, server_time}`.
use crate::lcnet::TipHub;
use crate::remote::Remote;
use serde_json::{Value, json};
use sm64coin_lc::proto::TipInfo;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

pub fn tip_json(t: &TipInfo) -> Value {
    json!({
        "node": "",
        "height": t.height,
        "hash": hex::encode(t.hash),
        "timestamp": t.timestamp,
        "cumulative_difficulty": t.cumulative_difficulty.to_string(),
        "difficulty": t.difficulty.to_string(),
        "server_time": t.server_time,
    })
}

/// Emits `tip` for every change of the hub's tip.
pub fn spawn_emitter(app: AppHandle, hub: Arc<TipHub>) {
    tauri::async_runtime::spawn(async move {
        let mut rx = hub.subscribe();
        while rx.changed().await.is_ok() {
            let t = rx.borrow().clone();
            if let Some(t) = t {
                let _ = app.emit("tip", tip_json(&hub.stamped(t)));
            }
        }
    });
}

/// The current tip (null until one is known): the page asks once when it subscribes, since events only tell changes.
#[tauri::command]
pub fn tip_get(remote: State<'_, Remote>) -> Option<Value> {
    let hub = remote.net.tip_hub();
    hub.current().map(|t| tip_json(&hub.stamped(t)))
}

/// Desktop: the app's own node, polled once a second (it is on this machine) for everyone who wants the tip: the page, the
/// verifier, the miner. `port` says where the node listens now (it may stop and start). Runs on its own thread for the app's life.
#[cfg(desktop)]
pub fn spawn_local_poll(hub: Arc<TipHub>, port: impl Fn() -> Option<u16> + Send + 'static) {
    use sm64coin_lc::exec::{header_timestamp, tip_from_info, tip_info_from};
    use std::time::Duration;
    std::thread::spawn(move || {
        let call = |p: u16, m: &str| sm64coin_launcher::rpc::call(p, m, json!({}), Duration::from_secs(5)).ok().and_then(|v| v.get("result").cloned());
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let Some(p) = port() else { continue };
            let Some(info) = call(p, "get_info") else { continue };
            let Some(tip) = tip_from_info(&info) else { continue };
            let timestamp = match hub.current().filter(|k| k.hash == tip.hash) {
                Some(k) => k.timestamp,
                None => match call(p, "get_last_block_header").and_then(|h| header_timestamp(&h, tip.hash)) {
                    Some(ts) => ts,
                    None => continue,
                },
            };
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            if let Some(t) = tip_info_from(&info, timestamp, now) {
                hub.offer_local(t);
            }
        }
    });
}
