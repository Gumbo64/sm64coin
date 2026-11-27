use crate::{AppError, Res};
use sm64coin_launcher::rpc;
use std::time::Duration;

/// The same whitelisted call against a remote node over https (the public node, when no local node answers; always on Android).
/// Done here and not with the webview's fetch so it needs no CORS entry on that node.
#[tauri::command]
pub async fn remote_rpc(url: String, method: String, params: Option<serde_json::Value>) -> Res<serde_json::Value> {
    tauri::async_runtime::spawn_blocking(move || rpc::call_url(&url, &method, params.unwrap_or_else(|| serde_json::json!({})), Duration::from_secs(60)).map_err(|e| AppError::new("rpc", e)))
        .await
        .map_err(|e| AppError::new("internal", e.to_string()))?
}
