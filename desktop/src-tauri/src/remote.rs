use crate::{AppError, Res};
use sm64coin_launcher::rpc;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// The website's public node (web/public/public-node.json, compiled in): the page cannot widen it.
const PUBLIC_NODE_JSON: &str = include_str!("../../../web/public/public-node.json");

/// `https://host[:port][/path]` without a trailing slash, or None when `u` is not such an address.
pub fn normalize(u: &str) -> Option<String> {
    let u = u.trim().trim_end_matches('/');
    let rest = u.strip_prefix("https://")?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || u.len() > 200 || u.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '@' | '?' | '#' | '\\' | '%')) {
        return None;
    }
    Some(u.to_string())
}

pub fn public_node() -> Option<String> {
    serde_json::from_str::<serde_json::Value>(PUBLIC_NODE_JSON).ok().and_then(|v| v.get("url").and_then(|u| u.as_str()).and_then(normalize))
}

/// The remote nodes `remote_rpc` (and the wallet's `set_daemon`) may reach: the public node, and one https node the user
/// confirmed in a native dialog (`remote_node_set`), kept in the data dir. Never an address the page passes per call.
pub struct Remote {
    public: Option<String>,
    saved: Mutex<Option<String>>,
    file: Option<PathBuf>,
}

impl Remote {
    pub fn new(file: Option<PathBuf>) -> Remote {
        let saved = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|s| normalize(&s));
        Remote { public: public_node(), saved: Mutex::new(saved), file }
    }
    /// The allowed form of `url`, or None.
    pub fn allowed(&self, url: &str) -> Option<String> {
        let u = normalize(url)?;
        (self.public.as_deref() == Some(u.as_str()) || self.saved.lock().unwrap().as_deref() == Some(u.as_str())).then_some(u)
    }
    pub fn saved(&self) -> Option<String> {
        self.saved.lock().unwrap().clone()
    }
    pub fn set_saved(&self, u: Option<String>) -> Res<()> {
        if let Some(f) = &self.file {
            let r = match &u {
                Some(u) => std::fs::write(f, u),
                None => std::fs::remove_file(f).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }),
            };
            r.map_err(|e| AppError::new("remote_node", e.to_string()))?;
        }
        *self.saved.lock().unwrap() = u;
        Ok(())
    }
}

#[derive(serde::Serialize)]
pub struct RemoteNodes {
    public: Option<String>,
    saved: Option<String>,
}

#[tauri::command]
pub fn remote_node_get(remote: tauri::State<'_, Remote>) -> Res<RemoteNodes> {
    Ok(RemoteNodes { public: remote.public.clone(), saved: remote.saved() })
}

/// The same whitelisted call against a remote node over https (the public node, when no local node answers; always on Android).
/// Done here and not with the webview's fetch so it needs no CORS entry on that node. Only the allowed nodes (see `Remote`).
#[tauri::command]
pub async fn remote_rpc(remote: tauri::State<'_, Remote>, url: String, method: String, params: Option<serde_json::Value>) -> Res<serde_json::Value> {
    let url = remote.allowed(&url).ok_or_else(|| AppError::new("forbidden", "that node is not the public node or the one saved on the Network page"))?;
    tauri::async_runtime::spawn_blocking(move || rpc::call_url(&url, &method, params.unwrap_or_else(|| serde_json::json!({})), Duration::from_secs(60)).map_err(|e| AppError::new("rpc", e)))
        .await
        .map_err(|e| AppError::new("internal", e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_public_and_the_saved_node() {
        let r = Remote { public: Some("https://eboy.au".into()), saved: Mutex::new(None), file: None };
        assert_eq!(r.allowed("https://eboy.au/").as_deref(), Some("https://eboy.au"));
        assert!(r.allowed("https://eboy.au.evil.example").is_none());
        assert!(r.allowed("https://evil.example").is_none());
        assert!(r.allowed("http://eboy.au").is_none());
        assert!(r.allowed("https://user@eboy.au").is_none());
        r.set_saved(normalize("https://node.example:18089")).unwrap();
        assert!(r.allowed("https://node.example:18089").is_some());
        assert!(r.allowed("https://node.example:18090").is_none());
        assert!(normalize("https:///x").is_none());
        assert!(public_node().is_some_and(|u| u.starts_with("https://")));
    }
}
