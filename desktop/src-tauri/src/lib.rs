//! SM64Coin app (Tauri 2): the website (web/, built with `--mode desktop`) in a webview, with Rust doing what a browser cannot.
//! The launcher library (pow/launcher) does the ROM/node/RPC work; the files here are the thin command layer the webview calls
//! (web/src/platform/tauri.js). Every command returns `Result<T, AppError>`.
//!
//! Two shapes, by Tauri's own `cfg(desktop)` / `cfg(mobile)` (set by tauri-build):
//! - desktop (`desktop.rs`, `miner.rs`): local node, ROM, native miner, wallet-rpc sidecar, passkey hand-off.
//! - mobile (Android APK): `remote_rpc` (the public node), `host_info`, and the native miner (`mobile_miner.rs`: the
//!   `sm64coin_miner` Campaign in this process, the ROM handed over once by the page with `rom_put`); no node, no sidecars.

mod remote;
#[cfg(desktop)]
mod desktop;
#[cfg(desktop)]
mod miner;
#[cfg(desktop)]
mod wallet;
#[cfg(desktop)]
mod link;
// The visualisation feed's checks (desktop: the miner's stdout; Android: the page's mailbox).
#[allow(dead_code)]
mod vizfeed;
// The Android miner's core: no Tauri types, so the host's tests drive it as the commands do.
#[cfg(any(mobile, test))]
#[cfg_attr(not(mobile), allow(dead_code))]
mod mobile_miner;

use serde::Serialize;

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

    /// The page's ROM (raw request body, from its IndexedDB): kept in the app's data dir after the SHA-1 check.
    #[tauri::command]
    pub async fn rom_put(miner: Miner<'_>, request: tauri::ipc::Request<'_>) -> Res<()> {
        match request.body() {
            tauri::ipc::InvokeBody::Raw(bytes) => miner.rom_put(bytes),
            _ => Err(AppError::new("bad_request", "rom_put takes the ROM's bytes as the request body")),
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

    #[tauri::command]
    pub fn miner_status(miner: Miner<'_>) -> Res<MinerInfo> {
        Ok(miner.status())
    }

    #[tauri::command]
    pub fn miner_viz_on(miner: Miner<'_>, on: bool) -> Res<()> {
        miner.viz_on(on)
    }

    #[tauri::command]
    pub fn miner_viz_mail(miner: Miner<'_>, mail: serde_json::Map<String, serde_json::Value>) -> Res<()> {
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
        .manage(remote::Remote::new(None))
        .setup(|t| {
            use tauri::Manager;
            let data = t.path().app_data_dir()?;
            t.manage(std::sync::Arc::new(mobile_miner::MobileMiner::new(data)));
            // tauri.conf.json's window has `create: false` (desktop.rs builds it, for portable mode): build it here too.
            if let Some(cfg) = t.config().app.windows.iter().find(|w| w.label == "main").cloned() {
                tauri::WebviewWindowBuilder::from_config(t.handle(), &cfg)?.build()?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![host_info, remote::remote_rpc, remote::remote_node_get, mobile_cmds::rom_put, mobile_cmds::miner_start, mobile_cmds::miner_stop, mobile_cmds::miner_status, mobile_cmds::miner_viz_on, mobile_cmds::miner_viz_mail])
        .run(tauri::generate_context!())
        .expect("error while running the SM64Coin app");
}
