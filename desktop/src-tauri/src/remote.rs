use crate::lcnet::{NetStatus, Net};
use crate::{AppError, Res};
use sm64coin_lc::nodes::{NodeAddr, parse_bootstrap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// The website's public node(s) (web/public/public-node.json, compiled in): the page cannot widen them. `{"url": ..}` or
/// `{"nodes": [..]}`, entries `https://host`.
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

/// The compiled-in bootstrap nodes (https ones are normalized like any address the page could pass).
pub fn bootstrap_nodes() -> Vec<NodeAddr> {
    bootstrap_from(PUBLIC_NODE_JSON)
}

fn bootstrap_from(json: &str) -> Vec<NodeAddr> {
    parse_bootstrap(json)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|NodeAddr::Http { url, tip }| normalize(&url).map(|url| NodeAddr::Http { url, tip }))
        .collect()
}

/// The first https public node (what the page shows as "the public node").
#[cfg(test)]
pub fn public_node() -> Option<String> {
    bootstrap_nodes().into_iter().next().map(|NodeAddr::Http { url, .. }| url)
}

/// The remote nodes `remote_rpc` (and the wallet's `set_daemon`) may reach: the public node(s), and one https node the user
/// confirmed in a native dialog (`remote_node_set`), kept in the data dir. Never an address the page passes per call.
/// Behind them is the app's light-client network (`Net`): a call for the public node is answered by whichever bootstrap or
/// learned node works (failover); a call for the user's own node goes to that node only.
pub struct Remote {
    public: Vec<String>,
    saved: Mutex<Option<String>>,
    file: Option<PathBuf>,
    pub net: Arc<Net>,
    /// The loopback daemon (walletproxy.rs) the native miner is given.
    proxy: OnceLock<String>,
}

impl Remote {
    /// `file`: where the user's confirmed node is kept; `nodes_file`: where the network keeps learned nodes.
    pub fn new(file: Option<PathBuf>, nodes_file: Option<PathBuf>) -> Remote {
        let saved = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|s| normalize(&s));
        Remote::with_net(file, saved.clone(), Net::new(&bootstrap_nodes(), saved.as_deref(), nodes_file))
    }
    pub fn with_net(file: Option<PathBuf>, saved: Option<String>, net: Arc<Net>) -> Remote {
        let public = bootstrap_nodes().into_iter().map(|NodeAddr::Http { url, .. }| url).collect();
        Remote { public, saved: Mutex::new(saved), file, net, proxy: OnceLock::new() }
    }
    fn is_public(&self, u: &str) -> bool {
        self.public.iter().any(|p| p == u)
    }
    /// The allowed form of `url`, or None.
    pub fn allowed(&self, url: &str) -> Option<String> {
        let u = normalize(url)?;
        (self.is_public(&u) || self.saved.lock().unwrap().as_deref() == Some(u.as_str())).then_some(u)
    }
    /// The daemon the native miner (Android) talks to: the loopback proxy, which fails over between the network's nodes (a
    /// campaign takes one fixed URL, so this is the only place that decides which node it may reach).
    #[cfg_attr(not(mobile), allow(dead_code))]
    pub fn daemon_url(&self) -> Res<String> {
        self.proxy.get().cloned().ok_or_else(|| AppError::new("no_public_node", "the app's node connection is not up yet"))
    }
    pub fn set_proxy(&self, url: &str) {
        let _ = self.proxy.set(url.to_string());
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
        let old = std::mem::replace(&mut *self.saved.lock().unwrap(), u.clone());
        self.net.set_saved(old.as_deref(), u.as_deref());
        Ok(())
    }
    /// The whitelisted call of `url` (an allowed node, see `allowed`): the user's own node is called alone, the public node
    /// through the network with failover.
    pub async fn rpc(&self, url: &str, method: &str, params: serde_json::Value) -> Res<serde_json::Value> {
        let u = self.allowed(url).ok_or_else(|| AppError::new("forbidden", "that node is not the public node or the one saved on the Network page"))?;
        // the user's own node alone; a call that carries the view key (sm64_board_join) alone to the node the page addressed
        let pinned = (!self.is_public(&u) && self.saved().as_deref() == Some(u.as_str())) || sm64coin_lc::allowlist::pinned_only(method);
        let r = if pinned { self.net.rpc_pinned(&u, method, params).await } else { self.net.rpc(method, params).await };
        r.map_err(|e| AppError::new("rpc", e))
    }
}

#[derive(serde::Serialize)]
pub struct RemoteNodes {
    public: Option<String>,
    saved: Option<String>,
}

#[tauri::command]
pub fn remote_node_get(remote: tauri::State<'_, Remote>) -> Res<RemoteNodes> {
    Ok(RemoteNodes { public: remote.public.first().cloned(), saved: remote.saved() })
}

/// The same whitelisted call against a remote node (the public node, when no local node answers; always on Android): through the
/// app's light-client network, so a dead node is skipped. Done here and not with the webview's fetch so it needs no CORS entry
/// on that node. Only the allowed nodes (see `Remote`).
#[tauri::command]
pub async fn remote_rpc(remote: tauri::State<'_, Remote>, url: String, method: String, params: Option<serde_json::Value>) -> Res<serde_json::Value> {
    remote.rpc(&url, &method, params.unwrap_or_else(|| serde_json::json!({}))).await
}

/// Which node and transport the app is using, the node list with its scores, the loopback daemon.
#[tauri::command]
pub fn net_status(remote: tauri::State<'_, Remote>) -> Res<NetStatus> {
    Ok(remote.net.status())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(saved: Option<String>) -> Remote {
        Remote::with_net(None, saved.clone(), Net::new(&[NodeAddr::http("https://eboy.au".into())], saved.as_deref(), None))
    }

    #[test]
    fn bootstrap_lists_https_entries_only() {
        let b = bootstrap_from(r#"{"url":"https://eboy.au","nodes":["https://eboy.au","https://evil.example@x","http://plain.example","iroh:abababababababababababababababababababababababababababababababab@https://relay.example"],"comment":"x"}"#);
        assert!(b.contains(&NodeAddr::http("https://eboy.au".into())));
        assert_eq!(b.len(), 2, "the url entry repeats nodes[0]; an old iroh entry and a userinfo host are dropped, plain http is not https: {b:?}");
        // the compiled-in file parses, with an https node for the page and every entry well-formed
        let c = bootstrap_nodes();
        assert!(c.iter().any(|a| matches!(a, NodeAddr::Http { .. })));
        let raw: serde_json::Value = serde_json::from_str(PUBLIC_NODE_JSON).unwrap();
        assert!(raw["nodes"].is_array() && raw["url"].is_string(), "old clients read url, new ones nodes");
        for n in raw["nodes"].as_array().unwrap() {
            let spec = n.as_str().or_else(|| n["url"].as_str()).unwrap();
            assert!(sm64coin_lc::nodes::parse_node_spec(spec).is_some(), "bad bootstrap entry {n}");
            if let Some(t) = n.get("tip") {
                assert!(sm64coin_lc::nodes::parse_tip_spec(t.as_str().unwrap()).is_some(), "bad tip in bootstrap entry {n}");
            }
        }
        // an entry's tip URL reaches the node list (an object entry, or the tips map the shipped file uses)
        let t = bootstrap_from(r#"{"nodes":[{"url":"https://eboy.au","tip":"https://eboy.au:16490/lc/tip"},"https://b.example"]}"#);
        assert_eq!((t[0].tip_url(), t[1].tip_url()), (Some("https://eboy.au:16490/lc/tip"), None));
        assert!(raw["nodes"].as_array().unwrap().iter().all(|n| n.is_string()), "releases before 0.5.6 read nodes as strings only");
        assert!(c.iter().any(|a| a.tip_url().is_some()), "the shipped tips map reaches the list");
    }

    #[test]
    fn only_the_public_and_the_saved_node() {
        let r = remote(None);
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
        // the confirmed node joins the network's list (above discovered ones) and leaves it when forgotten
        assert!(r.net.status().nodes.iter().any(|n| n.key == "http:https://node.example:18089"));
        r.set_saved(None).unwrap();
        assert!(!r.net.status().nodes.iter().any(|n| n.key.contains("node.example")));
    }
}
