//! SM64Coin app (Tauri 2): the website (web/, built with `--mode desktop`) in a webview, with Rust doing what a browser cannot.
//! The launcher library (pow/launcher) does the ROM/node/RPC work; the files here are the thin command layer the webview calls
//! (web/src/platform/tauri.js). Every command returns `Result<T, AppError>`.
//!
//! Two shapes, by Tauri's own `cfg(desktop)` / `cfg(mobile)` (set by tauri-build):
//! - desktop (`desktop.rs`, `miner.rs`): local node, ROM, native miner, wallet-rpc sidecar, passkey hand-off.
//! - mobile (Android APK): only `remote_rpc` (the public node) and `host_info`; no node, no sidecars, ROM through the page's
//!   own upload. A native miner for Android is future work (see desktop/README.md).

mod remote;
#[cfg(desktop)]
mod desktop;
#[cfg(desktop)]
mod miner;
#[cfg(desktop)]
mod wallet;
#[cfg(desktop)]
mod link;
#[cfg(desktop)]
mod vizfeed;

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
            // tauri.conf.json's window has `create: false` (desktop.rs builds it, for portable mode): build it here too.
            if let Some(cfg) = t.config().app.windows.iter().find(|w| w.label == "main").cloned() {
                tauri::WebviewWindowBuilder::from_config(t.handle(), &cfg)?.build()?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![host_info, remote::remote_rpc, remote::remote_node_get])
        .run(tauri::generate_context!())
        .expect("error while running the SM64Coin app");
}
