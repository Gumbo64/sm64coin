//! SM64Coin desktop: the website (web/, built with `--mode desktop`) in a Tauri window, with the node and the miner as
//! native child processes. The launcher library (pow/launcher) does the node/ROM/RPC work; this file is the thin
//! command layer the webview calls (see web/src/platform/tauri.js). Every command returns `Result<T, AppError>`.

mod miner;

use serde::Serialize;
use sm64coin_launcher::node::{APP_ORIGINS, Node, NodeConfig, NodeStatus, SITE_ORIGIN};
use sm64coin_launcher::util::{EXE, home};
use sm64coin_launcher::{rom, rpc, version};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

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
type Res<T> = Result<T, AppError>;

pub struct App {
    pub dirs: Dirs,
    node: Mutex<Option<Node>>,
    /// What went wrong last time the node was started (shown by node_status).
    node_error: Mutex<Option<String>>,
    /// A start is in progress (the DHT lookup takes seconds).
    starting: Mutex<bool>,
    pub miner: Mutex<Option<miner::Miner>>,
}

#[derive(Clone, Debug)]
pub struct Dirs {
    /// Where sm64coind, sm64coin-automine, rrtrun, seeds.txt, routes/ live (SM64COIN_BUNDLE, else next to the app).
    pub bundle: PathBuf,
    pub data: PathBuf,
}

impl Dirs {
    fn detect() -> Dirs {
        let exe_dir = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok()).and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_default();
        let bundle = std::env::var_os("SM64COIN_BUNDLE").map(PathBuf::from).unwrap_or(exe_dir);
        let data = std::env::var_os("SM64COIN_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| home().join(".sm64coin"));
        Dirs { bundle, data }
    }
    pub fn tool(&self, name: &str) -> PathBuf {
        self.bundle.join(format!("{name}{EXE}"))
    }
}

fn port_env(name: &str, default: u16) -> u16 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[derive(Serialize)]
struct RomInfo {
    found: bool,
    path: Option<String>,
    error: Option<String>,
}

fn rom_info(d: &Dirs) -> RomInfo {
    match rom::find(None, &d.bundle, &d.data) {
        Ok(p) => RomInfo { found: true, path: Some(p.display().to_string()), error: None },
        Err(e) => RomInfo { found: false, path: None, error: Some(e) },
    }
}

#[derive(Serialize)]
struct HostInfo {
    version: &'static str,
    platform: &'static str,
    data_dir: String,
    bundle_dir: String,
    has_node_binary: bool,
    has_miner_binary: bool,
    rom: RomInfo,
}

#[tauri::command]
fn host_info(app: State<'_, App>) -> Res<HostInfo> {
    let d = &app.dirs;
    Ok(HostInfo {
        version: version(),
        platform: std::env::consts::OS,
        data_dir: d.data.display().to_string(),
        bundle_dir: d.bundle.display().to_string(),
        has_node_binary: d.tool("sm64coind").is_file(),
        has_miner_binary: d.tool("sm64coin-automine").is_file(),
        rom: rom_info(d),
    })
}

fn rom_install(d: &Dirs, p: &Path) -> Res<RomInfo> {
    if !p.is_file() {
        return Err(AppError::new("rom_missing", format!("{} is not a file", p.display())));
    }
    match rom::verify(p) {
        Ok(true) => {}
        Ok(false) => return Err(AppError::new("rom_wrong", format!("{} is not the US Super Mario 64 ROM (SHA-1 {})", p.display(), rom::ROM_SHA1))),
        Err(e) => return Err(AppError::new("rom_unreadable", e)),
    }
    let dst = rom::install(p, &d.data).map_err(|e| AppError::new("rom_copy", e))?;
    Ok(RomInfo { found: true, path: Some(dst.display().to_string()), error: None })
}

/// After a ROM arrives the node can start (it did not when the app opened without one).
fn start_node_later(app: &AppHandle) {
    let h = app.clone();
    std::thread::spawn(move || {
        if let Err(e) = start_node(&h.state::<App>()) {
            eprintln!("node: {}", e.message);
        }
    });
}

#[tauri::command]
fn rom_set(app: AppHandle, path: String) -> Res<RomInfo> {
    let r = rom_install(&app.state::<App>().dirs, Path::new(path.trim()))?;
    start_node_later(&app);
    Ok(r)
}

/// Opens the system file dialog and installs the chosen ROM; `None` when the dialog was cancelled.
#[tauri::command]
async fn rom_pick(app: AppHandle) -> Res<Option<RomInfo>> {
    use tauri_plugin_dialog::DialogExt;
    let h = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || h.dialog().file().set_title("Super Mario 64 (US) ROM").add_filter("N64 ROM", &["z64", "n64", "v64", "rom"]).add_filter("All files", &["*"]).blocking_pick_file())
        .await
        .map_err(|e| AppError::new("internal", e.to_string()))?;
    let Some(f) = picked else { return Ok(None) };
    let p = f.into_path().map_err(|e| AppError::new("rom_missing", e.to_string()))?;
    let r = rom_install(&app.state::<App>().dirs, &p)?;
    start_node_later(&app);
    Ok(Some(r))
}

/// The ROM's bytes for the in-page game (which keeps its own copy in IndexedDB), so the app never asks for a second upload.
#[tauri::command]
async fn rom_bytes(app: AppHandle) -> Res<tauri::ipc::Response> {
    let d = app.state::<App>().dirs.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let p = rom::find(None, &d.bundle, &d.data).map_err(|e| AppError::new("rom_missing", e))?;
        std::fs::read(&p).map(tauri::ipc::Response::new).map_err(|e| AppError::new("rom_unreadable", e.to_string()))
    })
    .await
    .map_err(|e| AppError::new("internal", e.to_string()))?
}

fn node_config(d: &Dirs, rom: PathBuf) -> NodeConfig {
    let mut c = NodeConfig::new(d.tool("sm64coind"), rom, d.data.clone());
    c.seed_files = vec![d.bundle.join("seeds.txt"), d.data.join("seeds.txt")];
    c.p2p_port = port_env("SM64COIN_P2P_PORT", 16480);
    c.rpc_port = port_env("SM64COIN_RPC_PORT", 16481);
    c.zmq_port = port_env("SM64COIN_ZMQ_PORT", 16482);
    c.no_dht = std::env::var_os("SM64COIN_NO_DHT").is_some();
    c.origins = Some(format!("{SITE_ORIGIN},{APP_ORIGINS}"));
    c
}

#[derive(Serialize)]
struct NodeInfo {
    #[serde(flatten)]
    status: NodeStatus,
    /// A start is in progress (DHT lookup, spawn).
    starting: bool,
    /// Why the node is not running, if the last start failed.
    error: Option<String>,
    /// The RPC address the webview's wallet may use directly (CORS is allowed for the app's origins).
    rpc_url: Option<String>,
}

fn node_info(app: &App) -> NodeInfo {
    let starting = *app.starting.lock().unwrap();
    let error = app.node_error.lock().unwrap().clone();
    let mut g = app.node.lock().unwrap();
    match g.as_mut() {
        Some(n) => {
            let st = n.status();
            NodeInfo { rpc_url: Some(format!("http://127.0.0.1:{}", n.rpc_port)), status: st, starting, error }
        }
        None => NodeInfo { status: NodeStatus::default(), starting, error, rpc_url: None },
    }
}

/// Starts the node (blocking: DHT lookup and spawn). The node answering is `node_status`'s business.
fn start_node(app: &App) -> Res<()> {
    {
        let mut s = app.starting.lock().unwrap();
        if *s || app.node.lock().unwrap().as_mut().is_some_and(|n| !n.died()) {
            return Ok(());
        }
        *s = true;
    }
    let r = (|| {
        let rom = rom::find(None, &app.dirs.bundle, &app.dirs.data).map_err(|e| AppError::new("rom_missing", e))?;
        let cfg = node_config(&app.dirs, rom);
        if !cfg.daemon_bin.is_file() {
            return Err(AppError::new("node_binary_missing", format!("{} is missing", cfg.daemon_bin.display())));
        }
        let node = Node::start(&cfg).map_err(|e| AppError::new("node_start", e))?;
        *app.node.lock().unwrap() = Some(node);
        Ok(())
    })();
    *app.node_error.lock().unwrap() = r.as_ref().err().map(|e| e.message.clone());
    *app.starting.lock().unwrap() = false;
    r
}

#[tauri::command]
async fn node_start(app: AppHandle) -> Res<NodeInfo> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<App>();
        start_node(&st)?;
        Ok(node_info(&st))
    })
    .await
    .map_err(|e| AppError::new("internal", e.to_string()))?
}

#[tauri::command]
async fn node_stop(app: AppHandle) -> Res<NodeInfo> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<App>();
        let n = st.node.lock().unwrap().take();
        if let Some(mut n) = n {
            n.stop();
        }
        Ok(node_info(&st))
    })
    .await
    .map_err(|e| AppError::new("internal", e.to_string()))?
}

#[tauri::command]
async fn node_status(app: AppHandle) -> Res<NodeInfo> {
    tauri::async_runtime::spawn_blocking(move || Ok(node_info(&app.state::<App>()))).await.map_err(|e| AppError::new("internal", e.to_string()))?
}

/// Forwards a whitelisted JSON-RPC call to the local node and returns the whole JSON-RPC answer
/// (the webview then needs no CORS and cannot reach the node's other methods).
#[tauri::command]
async fn node_rpc(app: AppHandle, method: String, params: Option<serde_json::Value>) -> Res<serde_json::Value> {
    if !rpc::allowed(&method) {
        return Err(AppError::new("forbidden", format!("RPC method {method} is not available")));
    }
    let port = {
        let st = app.state::<App>();
        let g = st.node.lock().unwrap();
        g.as_ref().map(|n| n.rpc_port).ok_or_else(|| AppError::new("node_not_running", "the local node is not running"))?
    };
    tauri::async_runtime::spawn_blocking(move || rpc::call(port, &method, params.unwrap_or_else(|| serde_json::json!({})), Duration::from_secs(60)).map_err(|e| AppError::new("rpc", e)))
        .await
        .map_err(|e| AppError::new("internal", e.to_string()))?
}

/// The same whitelisted call against a remote node over https (the public node, when no local node answers). Done here and not
/// with the webview's fetch so it needs no CORS entry on that node.
#[tauri::command]
async fn remote_rpc(url: String, method: String, params: Option<serde_json::Value>) -> Res<serde_json::Value> {
    tauri::async_runtime::spawn_blocking(move || rpc::call_url(&url, &method, params.unwrap_or_else(|| serde_json::json!({})), Duration::from_secs(60)).map_err(|e| AppError::new("rpc", e)))
        .await
        .map_err(|e| AppError::new("internal", e.to_string()))?
}

#[tauri::command]
fn miner_start(app: AppHandle, state: State<'_, App>, address: String, threads: Option<usize>) -> Res<miner::MinerInfo> {
    let rpc_port = {
        let g = state.node.lock().unwrap();
        g.as_ref().map(|n| n.rpc_port).ok_or_else(|| AppError::new("node_not_running", "start the local node first"))?
    };
    let rom = rom::find(None, &state.dirs.bundle, &state.dirs.data).map_err(|e| AppError::new("rom_missing", e))?;
    let mut g = state.miner.lock().unwrap();
    if let Some(m) = g.as_mut() {
        if m.alive() {
            return Err(AppError::new("already_running", "the native miner is already running"));
        }
    }
    let m = miner::Miner::start(&app, &state.dirs, &rom, rpc_port, address.trim(), threads)?;
    let mut m = m; let info = m.info();
    *g = Some(m);
    Ok(info)
}

#[tauri::command]
async fn miner_stop(app: AppHandle) -> Res<miner::MinerInfo> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<App>();
        let m = st.miner.lock().unwrap().take();
        if let Some(mut m) = m {
            m.stop();
            let _ = app.emit("miner-state", m.info());
            return Ok(m.info());
        }
        Ok(miner::MinerInfo::idle())
    })
    .await
    .map_err(|e| AppError::new("internal", e.to_string()))?
}

#[tauri::command]
fn miner_status(state: State<'_, App>) -> Res<miner::MinerInfo> {
    Ok(state.miner.lock().unwrap().as_mut().map(|m| m.info()).unwrap_or_else(miner::MinerInfo::idle))
}

pub fn run() {
    let dirs = Dirs::detect();
    let app = App { dirs, node: Mutex::new(None), node_error: Mutex::new(None), starting: Mutex::new(false), miner: Mutex::new(None) };
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(app)
        .setup(|t| {
            // The node starts with the app when the ROM is there (the UI asks for it otherwise, then calls node_start).
            // SIGTERM / SIGINT / console close: leave through the normal exit path, which stops the miner and the node.
            let hx = t.handle().clone();
            let _ = ctrlc::set_handler(move || hx.exit(0));
            let h = t.handle().clone();
            std::thread::spawn(move || {
                let st = h.state::<App>();
                if rom::find(None, &st.dirs.bundle, &st.dirs.data).is_ok() {
                    if let Err(e) = start_node(&st) {
                        eprintln!("node: {}", e.message);
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![host_info, rom_set, rom_pick, rom_bytes, node_start, node_stop, node_status, node_rpc, remote_rpc, miner_start, miner_stop, miner_status])
        .build(tauri::generate_context!())
        .expect("error while building the SM64Coin app")
        .run(|h, ev| {
            if let tauri::RunEvent::Exit = ev {
                // Reverse order: miner first, then the node (its stop waits for a clean database close).
                let st = h.state::<App>();
                if let Some(mut m) = st.miner.lock().unwrap().take() {
                    m.stop();
                }
                if let Some(mut n) = st.node.lock().unwrap().take() {
                    n.stop();
                }
            }
        });
}
