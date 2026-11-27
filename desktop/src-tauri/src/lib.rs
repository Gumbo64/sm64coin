//! SM64Coin app (Tauri 2): the website (web/, built with `--mode desktop`) in a webview, with Rust doing what a browser cannot.
//! The launcher library (pow/launcher) does the ROM/node/RPC work; the files here are the thin command layer the webview calls
//! (web/src/platform/tauri.js). Every command returns `Result<T, AppError>`.
//!
//! Two shapes, by Tauri's own `cfg(desktop)` / `cfg(mobile)` (set by tauri-build):
//! - desktop (`desktop.rs`, `miner.rs`): local node, ROM, native miner, wallet-rpc sidecar, passkey hand-off.
//! - mobile (Android APK): `remote_rpc` (the public node), `host_info`, and the native miner (`mobile_miner.rs`: the
//!   `sm64coin_miner` Campaign in this process, the ROM handed over once by the page with `rom_put`); no node, no sidecars.

mod lcnet;
mod lctip;
mod lcverify;
mod remote;
mod walletproxy;
#[cfg(test)]
mod lcnet_test;
#[cfg(desktop)]
mod desktop;
#[cfg(desktop)]
mod miner;
#[cfg(desktop)]
mod wallet;
mod link; // desktop: the passkey hand-off into wallet-rpc; Android: the seed goes to the page's WASM wallet (link_take)
// The visualisation feed's checks (desktop: the miner's stdout; Android: the page's mailbox).
#[allow(dead_code)]
mod vizfeed;
// The Android miner's core: no Tauri types, so the host's tests drive it as the commands do.
#[cfg(any(mobile, test))]
#[cfg_attr(not(mobile), allow(dead_code))]
mod mobile_miner;

use serde::Serialize;

/// Starts the app's light-client network: the loopback daemon (walletproxy.rs) for the wallet and the native miner, and the
/// background work (iroh endpoint, node gossip, persistence). The returned proxy must be kept (managed) while the app runs.
pub(crate) fn start_net(remote: &remote::Remote) -> Result<std::sync::Arc<walletproxy::WalletProxy>, String> {
    let proxy = walletproxy::start(remote.net.clone(), vec![])?;
    remote.set_proxy(&proxy.url());
    remote.net.spawn_background();
    Ok(proxy)
}

/// A typed error: `code` is stable (the UI switches on it), `message` is for people.
#[derive(Debug, Serialize)]
pub struct AppError {
    pub code: &'static str,
    pub message: String,
}
impl AppError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        AppError { code, message: message.into() }
    }
}
pub type Res<T> = Result<T, AppError>;

#[cfg(mobile)]
#[derive(Serialize)]
struct MobileHost {
    version: &'static str,
    platform: &'static str,
    mobile: bool,
}

#[cfg(mobile)]
#[tauri::command]
fn host_info() -> Res<MobileHost> {
    Ok(MobileHost { version: sm64coin_launcher::version(), platform: std::env::consts::OS, mobile: true })
}

#[cfg(mobile)]
mod mobile_cmds {
    use crate::mobile_miner::{MinerInfo, MobileMiner};
    use crate::remote::Remote;
    use crate::{AppError, Res};
    use std::sync::Arc;
    use tauri::{AppHandle, Emitter, State};

    type Miner<'a> = State<'a, Arc<MobileMiner>>;

    fn emitter(app: AppHandle) -> crate::mobile_miner::Emit {
        Arc::new(move |name, v| {
            let _ = app.emit(name, v);
        })
    }

    /// The page's ROM (from its IndexedDB): kept in the app's data dir after the size and SHA-1 checks. Android's IPC has no raw
    /// bodies (Tauri: "On Android, InvokeBody::Raw is not supported"), so the page sends `{rom: "<base64>"}`; a raw body is
    /// taken too.
    #[tauri::command]
    pub async fn rom_put(miner: Miner<'_>, request: tauri::ipc::Request<'_>) -> Res<()> {
        match request.body() {
            tauri::ipc::InvokeBody::Raw(bytes) => miner.rom_put(bytes),
            tauri::ipc::InvokeBody::Json(v) => match v.get("rom").and_then(|r| r.as_str()) {
                Some(b64) => miner.rom_put_b64(b64),
                None => Err(AppError::new("bad_request", "rom_put takes {rom: the ROM's bytes in base64}")),
            },
        }
    }

    /// Starts mining against the public node (never an address from the page).
    #[tauri::command]
    pub async fn miner_start(app: AppHandle, miner: Miner<'_>, remote: State<'_, Remote>, address: String, threads: Option<usize>, viz: Option<bool>) -> Res<MinerInfo> {
        let daemon = remote.daemon_url()?;
        let (m, emit) = (miner.inner().clone(), emitter(app));
        tauri::async_runtime::spawn_blocking(move || m.start(emit, &daemon, address.trim(), threads, viz.unwrap_or(true)))
            .await
            .map_err(|e| AppError::new("internal", e.to_string()))?
    }

    #[tauri::command]
    pub async fn miner_stop(app: AppHandle, miner: Miner<'_>) -> Res<MinerInfo> {
        let (m, emit) = (miner.inner().clone(), emitter(app));
        tauri::async_runtime::spawn_blocking(move || m.stop(&emit)).await.map_err(|e| AppError::new("internal", e.to_string()))
    }

    // Not on the main thread (a sync command's place): `miner_start` holds the miner's lock while the game's WebAssembly
    // compiles (seconds on a phone), and the page polls the status every 2 s.
    #[tauri::command]
    pub async fn miner_status(miner: Miner<'_>) -> Res<MinerInfo> {
        Ok(miner.status())
    }

    #[tauri::command]
    pub async fn miner_viz_on(miner: Miner<'_>, on: bool) -> Res<()> {
        miner.viz_on(on)
    }

    #[tauri::command]
    pub async fn miner_viz_mail(miner: Miner<'_>, mail: serde_json::Map<String, serde_json::Value>) -> Res<()> {
        miner.viz_mail(&mail)
    }
}

#[cfg(desktop)]
pub fn run() {
    desktop::run();
}

/// Android: the window never leaves the app's own pages (http(s)://tauri.localhost); a link to a web page opens in the
/// system browser instead (as desktop.rs `nav_guard` does), so no remote page is shown inside the app's window.
#[cfg(mobile)]
fn nav_guard<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    use tauri::Manager;
    use tauri_plugin_opener::OpenerExt;
    tauri::plugin::Builder::new("nav-guard")
        .on_navigation(|w, url| {
            let own = (matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost")) || url.scheme() == "tauri" || url.as_str() == "about:blank";
            if own {
                return true;
            }
            if matches!(url.scheme(), "http" | "https") {
                let _ = w.app_handle().opener().open_url(url.as_str(), None::<&str>);
            }
            false
        })
        .build()
}

#[cfg(mobile)]
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(nav_guard())
        .manage(link::Link::default())
        .setup(|t| {
            use tauri::Manager;
            let data = t.path().app_data_dir()?;
            let remote = remote::Remote::new(None, Some(data.join("nodes.json")));
            t.manage(start_net(&remote)?);
            let tips = remote.net.tip_hub();
            lctip::spawn_emitter(t.handle().clone(), tips.clone()); // the page's `tip` events
            t.manage(remote);
            let mm = std::sync::Arc::new(mobile_miner::MobileMiner::new(data));
            mm.set_tip_feed(std::sync::Arc::new(mobile_miner::HubFeed(tips)));
            t.manage(mm);
            // tauri.conf.json's window has `create: false` (desktop.rs builds it, for portable mode): build it here too.
            if let Some(cfg) = t.config().app.windows.iter().find(|w| w.label == "main").cloned() {
                tauri::WebviewWindowBuilder::from_config(t.handle(), &cfg)?.build()?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![host_info, remote::remote_rpc, remote::remote_node_get, remote::net_status, lctip::tip_get, mobile_cmds::rom_put, mobile_cmds::miner_start, mobile_cmds::miner_stop, mobile_cmds::miner_status, mobile_cmds::miner_viz_on, mobile_cmds::miner_viz_mail, link::link_start, link::link_status, link::link_cancel, link::link_take])
        .run(tauri::generate_context!())
        .expect("error while running the SM64Coin app");
}
