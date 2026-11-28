//! The desktop half of the app: local node, ROM, native miner (and wallet-rpc): everything that needs processes and files.
//! Compiled for desktop targets only (`cfg(desktop)`, see lib.rs); Android has the remote RPC and nothing else.

use crate::{link, miner, wallet};
use crate::{AppError, Res};
use serde::{Deserialize, Serialize};
use sm64coin_launcher::node::{probe_status, APP_ORIGINS, Node, NodeConfig, NodeStatus, SITE_ORIGIN};
use sm64coin_launcher::util::{EXE, home};
use sm64coin_launcher::{rom, rpc, version};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

pub struct App {
    pub dirs: Dirs,
    node: Mutex<Option<Node>>,
    /// What went wrong last time the node was started (shown by node_status).
    node_error: Mutex<Option<String>>,
    /// A start is in progress (the DHT lookup takes seconds).
    starting: Mutex<bool>,
    /// The NAT-PMP/PCP/UPnP mapping the user detected for the P2P port, renewed while the app runs.
    portmap: Mutex<Option<sm64coin_launcher::portmap::Mapping>>,
    /// One renewing thread per app.
    renewing: AtomicBool,
    pub miner: Mutex<Option<miner::Miner>>,
    pub wallet: wallet::Wallet,
    pub link: link::Link,
}

impl App {
    /// The RPC port of the node this app started, if any.
    pub fn node_rpc_port(&self) -> Option<u16> {
        self.node.lock().unwrap().as_ref().map(|n| n.rpc_port)
    }
}

#[derive(Clone, Debug)]
pub struct Dirs {
    /// Where sm64coind, sm64coin-automine, rrtrun, seeds.txt, routes/ live (SM64COIN_BUNDLE, else next to the app).
    pub bundle: PathBuf,
    /// Where seeds.txt and routes/ live: the bundle folder, or (installed app) the resource folder.
    pub res: PathBuf,
    pub data: PathBuf,
    /// Portable mode (a file named `portable` next to the program): `data` is the `data/` folder beside it, and so is the
    /// webview's own storage (the page's settings).
    pub portable: bool,
}

/// The marker file next to the program that switches portable mode on.
pub const PORTABLE_MARK: &str = "portable";

/// Where the app keeps its data (chain, wallets, ROM copy, settings, remote-node.txt), and whether that is portable mode:
/// `SM64COIN_DATA_DIR` if set; else `<exe dir>/data` when a file named `portable` sits next to the program; else `~/.sm64coin`.
pub fn data_dir(env: Option<std::ffi::OsString>, exe_dir: &Path, home: &Path) -> (PathBuf, bool) {
    if let Some(d) = env.filter(|d| !d.is_empty()) {
        return (PathBuf::from(d), false);
    }
    if exe_dir.join(PORTABLE_MARK).is_file() {
        return (exe_dir.join("data"), true);
    }
    (home.join(".sm64coin"), false)
}

impl Dirs {
    fn detect() -> Dirs {
        let exe_dir = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok()).and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_default();
        let (data, portable) = data_dir(std::env::var_os("SM64COIN_DATA_DIR"), &sm64coin_launcher::util::plain(exe_dir.clone()), &home());
        let bundle = std::env::var_os("SM64COIN_BUNDLE").map(PathBuf::from).unwrap_or(exe_dir);
        // Installed app: sidecars sit next to the program (Linux /usr/bin, Windows and macOS: the app folder) and the resources
        // are /usr/lib/SM64Coin (deb, AppImage), the install folder (Windows) or Contents/Resources (macOS).
        let exe = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok()).and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_default();
        let res = [bundle.clone(), exe.join("../lib/SM64Coin"), exe.join("../Resources")].into_iter().find(|d| d.join("seeds.txt").is_file()).map(|d| sm64coin_launcher::util::plain(d.canonicalize().unwrap_or(d))).unwrap_or_else(|| bundle.clone());
        Dirs { bundle, res, data, portable }
    }
    pub fn tool(&self, name: &str) -> PathBuf {
        self.bundle.join(format!("{name}{EXE}"))
    }
}

pub fn port_env(name: &str, default: u16) -> u16 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// The node settings the user can change before/after a start (kept in `<data>/node.json`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeSettings {
    /// The local P2P listening port: the port a VPN's forward must reach.
    pub p2p_port: u16,
    /// The public P2P port peers are told (`--p2p-external-port`); 0 means the same as `p2p_port`.
    pub p2p_external_port: u16,
    /// Start the node when the app opens (and after a ROM is installed).
    pub auto_start: bool,
}

impl Default for NodeSettings {
    fn default() -> Self {
        NodeSettings { p2p_port: port_env("SM64COIN_P2P_PORT", 16480), p2p_external_port: 0, auto_start: true }
    }
}

fn settings_path(d: &Dirs) -> PathBuf {
    d.data.join("node.json")
}

fn load_settings(d: &Dirs) -> NodeSettings {
    let s = std::fs::read_to_string(settings_path(d)).ok().and_then(|t| serde_json::from_str::<NodeSettings>(&t).ok()).unwrap_or_default();
    // A hand-edited file must not stop the app from starting; fix the one value that would make no sense (a privileged port).
    NodeSettings { p2p_port: if s.p2p_port < 1024 { NodeSettings::default().p2p_port } else { s.p2p_port }, ..s }
}

fn validate_settings(s: &NodeSettings) -> Res<()> {
    if s.p2p_port < 1024 {
        return Err(AppError::new("bad_port", "the P2P port must be 1024 or higher (ports below need administrator rights)"));
    }
    if s.p2p_external_port != 0 && s.p2p_external_port < 1024 {
        return Err(AppError::new("bad_port", "the public port must be 0 (same as P2P) or 1024 or higher"));
    }
    Ok(())
}

fn save_settings(d: &Dirs, s: &NodeSettings) -> Res<()> {
    validate_settings(s)?;
    std::fs::create_dir_all(&d.data).map_err(|e| AppError::new("settings_write", format!("cannot create {}: {e}", d.data.display())))?;
    let body = serde_json::to_vec_pretty(s).map_err(|e| AppError::new("settings_write", e.to_string()))?;
    std::fs::write(settings_path(d), body).map_err(|e| AppError::new("settings_write", format!("cannot write {}: {e}", settings_path(d).display())))
}

#[tauri::command]
fn node_settings(app: State<'_, App>) -> Res<NodeSettings> {
    Ok(load_settings(&app.dirs))
}

/// Saves the settings. A running node keeps the ports it started with: the page offers to restart it (the frontend does
/// stop + start, so the user sees the two steps and their errors).
#[tauri::command]
fn node_settings_set(app: State<'_, App>, settings: NodeSettings) -> Res<NodeSettings> {
    save_settings(&app.dirs, &settings)?;
    Ok(settings)
}

/// Asks the default gateway (the VPN's tunnel gateway when the VPN owns the default route) for a mapping of the P2P port in
/// the page's unsaved form (default: the saved one), trying NAT-PMP, PCP and UPnP-IGD. Gateways that implement one of them
/// expose the forwarded port this way; providers that do not make this return a clear error, and the page asks the user for
/// the port shown in the VPN app.
#[tauri::command]
async fn node_port_detect(app: AppHandle, port: Option<u16>) -> Res<sm64coin_launcher::portmap::Mapping> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<App>();
        let internal = port.unwrap_or_else(|| load_settings(&st.dirs).p2p_port);
        let mapping = sm64coin_launcher::portmap::detect(internal).map_err(|e| AppError::new("port_detect", e))?;
        *st.portmap.lock().unwrap() = Some(mapping.clone());
        if !st.renewing.swap(true, Ordering::SeqCst) {
            spawn_portmap_renewer(app.clone());
        }
        Ok(mapping)
    })
    .await
    .map_err(|e| AppError::new("internal", e.to_string()))?
}

/// Port mappings can be short-lived: renew the detected one for as long as the app runs.
fn spawn_portmap_renewer(app: AppHandle) {
    std::thread::spawn(move || loop {
        let st = app.state::<App>();
        let current = st.portmap.lock().unwrap().clone();
        let Some(mapping) = current else {
            std::thread::sleep(Duration::from_secs(30));
            continue;
        };
        std::thread::sleep(sm64coin_launcher::portmap::renew_every(&mapping));
        let st = app.state::<App>();
        match sm64coin_launcher::portmap::renew(&mapping) {
            Ok(next) => {
                if next.external_port != mapping.external_port {
                    eprintln!(
                        "port forward: the public port changed from {} to {}; set it on the Node page and restart the node",
                        mapping.external_port, next.external_port
                    );
                }
                *st.portmap.lock().unwrap() = Some(next);
            }
            Err(e) => eprintln!("port forward: renewal failed: {e}"),
        }
    });
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
    /// Portable mode: data/ next to the program (a file named `portable` beside it).
    portable: bool,
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
        portable: d.portable,
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
        let st = h.state::<App>();
        if !load_settings(&st.dirs).auto_start {
            return;
        }
        if let Err(e) = start_node(&st) {
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
    let s = load_settings(d);
    let mut c = NodeConfig::new(d.tool("sm64coind"), rom, d.data.clone());
    c.seed_files = vec![d.res.join("seeds.txt"), d.data.join("seeds.txt")];
    c.p2p_port = s.p2p_port;
    c.p2p_external_port = s.p2p_external_port;
    c.strict_p2p_port = true; // a VPN forward points at this exact port: never silently move it
    c.rpc_port = port_env("SM64COIN_RPC_PORT", 16481);
    c.zmq_port = port_env("SM64COIN_ZMQ_PORT", 16482);
    // The app has the release's seeds and the node's own peer exchange; a 20 s DHT lookup before the daemon starts only
    // makes the window feel stuck (and often stalls behind a VPN). The CLI launcher keeps the DHT.
    c.no_dht = std::env::var_os("SM64COIN_DHT").is_none();
    c.origins = Some(format!("{SITE_ORIGIN},{APP_ORIGINS}"));
    c
}

/// The ports shown while the node is stopped: the saved P2P settings and the configured local RPC/ZMQ ports.
fn stopped_status(d: &Dirs) -> NodeStatus {
    let s = load_settings(d);
    NodeStatus { p2p_port: s.p2p_port, p2p_external_port: s.p2p_external_port, rpc_port: port_env("SM64COIN_RPC_PORT", 16481), zmq_port: port_env("SM64COIN_ZMQ_PORT", 16482), ..Default::default() }
}

#[derive(Serialize)]
struct NodeInfo {
    #[serde(flatten)]
    status: NodeStatus,
    /// A start is in progress (spawn, or waiting for the ROM).
    starting: bool,
    /// Why the node is not running, if the last start failed.
    error: Option<String>,
    /// The RPC address the webview's wallet may use directly (CORS is allowed for the app's origins).
    rpc_url: Option<String>,
}

fn node_info(app: &App) -> NodeInfo {
    let starting = *app.starting.lock().unwrap();
    let error = app.node_error.lock().unwrap().clone();
    // Take the ownership/port fields under the lock, then release it before the RPC probe: a node loading its chain can take a
    // second to answer, and holding the node mutex that long would stall the miner, the wallet and every other status poll.
    let (mut st, rpc_port) = {
        let mut g = app.node.lock().unwrap();
        match g.as_mut() {
            Some(n) => (n.status_base(), Some(n.rpc_port)),
            None => (stopped_status(&app.dirs), None),
        }
    };
    if let Some(port) = rpc_port {
        probe_status(&mut st, port);
    }
    NodeInfo { rpc_url: rpc_port.map(|p| format!("http://127.0.0.1:{p}")), status: st, starting, error }
}

/// The tail of the node's logs: the daemon's own log (`node-daemon.log`) and the child's stdout/stderr (`node.log`).
#[derive(Serialize)]
struct NodeLogs {
    dir: String,
    daemon: Option<String>,
    app: Option<String>,
}

/// The last `lines` lines of a file, reading at most 256 KiB from its end (logs grow; never load the whole file).
fn tail_text(path: &Path, lines: usize) -> Option<String> {
    const MAX: u64 = 256 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(MAX);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = sm64coin_launcher::util::strip_ansi(&String::from_utf8_lossy(&buf));
    let mut all: Vec<&str> = text.lines().collect();
    if start > 0 && !all.is_empty() {
        all.remove(0); // the first line is a fragment of the line the read started in
    }
    let from = all.len().saturating_sub(lines);
    Some(all[from..].join("\n"))
}

#[tauri::command]
async fn node_log(app: AppHandle, lines: Option<usize>) -> Res<NodeLogs> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = app.state::<App>().dirs.data.join("logs");
        let n = lines.unwrap_or(200).clamp(1, 2000);
        Ok(NodeLogs { dir: dir.display().to_string(), daemon: tail_text(&dir.join("node-daemon.log"), n), app: tail_text(&dir.join("node.log"), n) })
    })
    .await
    .map_err(|e| AppError::new("internal", e.to_string()))?
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

/// Saves the https node the user picked on the Network page as the one `remote_rpc` and the wallet may also use, after the user
/// confirms it in a native dialog (the page cannot widen the list by itself). `None` forgets it (no question asked).
#[tauri::command]
async fn remote_node_set(app: AppHandle, url: Option<String>) -> Res<Option<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        let remote = app.state::<crate::remote::Remote>();
        let Some(url) = url.filter(|u| !u.trim().is_empty()) else {
            remote.set_saved(None)?;
            return Ok(None);
        };
        let u = crate::remote::normalize(&url).ok_or_else(|| AppError::new("bad_url", "a remote node must be an https:// address"))?;
        if remote.allowed(&u).is_some() {
            return Ok(Some(u));
        }
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
        let ok = app
            .dialog()
            .message(format!("Read the chain from the node at\n\n{u}\n\nand send your wallet's requests there? That node sees which blocks and transactions this app asks about."))
            .title("Use this node?")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom("Use it".into(), "Cancel".into()))
            .blocking_show();
        if !ok {
            return Err(AppError::new("cancelled", "the node was not confirmed"));
        }
        remote.set_saved(Some(u.clone()))?;
        Ok(Some(u))
    })
    .await
    .map_err(|e| AppError::new("internal", e.to_string()))?
}

#[tauri::command]
fn miner_start(app: AppHandle, state: State<'_, App>, address: String, threads: Option<usize>, viz: Option<bool>) -> Res<miner::MinerInfo> {
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
    let m = miner::Miner::start(&app, &state.dirs, &rom, rpc_port, address.trim(), threads, viz.unwrap_or(true))?;
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

/// The page switches the miner's visualisation feed on or off (the search keeps running; off it only counts frames).
#[tauri::command]
fn miner_viz_on(state: State<'_, App>, on: bool) -> Res<()> {
    match state.miner.lock().unwrap().as_ref() {
        Some(m) => m.viz_on(on),
        None => Ok(()),
    }
}

/// The page's mailbox for the running searches (pow/miner/src/rrt/vizio.rs): an object with any of `goal` (the sampling
/// bubble {level, area, pos, radius, share} or null), `cmds` (hybrid commands: node state requests, played segments) and
/// `reqs` (node path requests). Keys replace; the miner keeps the latest and gives it to every step.
#[tauri::command]
fn miner_viz_mail(state: State<'_, App>, mail: serde_json::Map<String, serde_json::Value>) -> Res<()> {
    match state.miner.lock().unwrap().as_ref() {
        Some(m) => m.mail(&mail),
        None => Err(AppError::new("miner_not_running", "the native miner is not running")),
    }
}

#[tauri::command]
fn miner_status(state: State<'_, App>) -> Res<miner::MinerInfo> {
    Ok(state.miner.lock().unwrap().as_mut().map(|m| m.info()).unwrap_or_else(miner::MinerInfo::idle))
}

/// Is `url` the app's own page (tauri://localhost on Linux/macOS, http(s)://tauri.localhost on Windows)?
fn app_url(url: &tauri::Url) -> bool {
    url.scheme() == "tauri" || (matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost")) || url.as_str() == "about:blank"
}

/// The window never leaves the app's own pages: a link to a web page opens in the system browser instead (the app's
/// capabilities only apply to its own origin anyway; this keeps a remote page from posing as the app).
fn nav_guard<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("nav-guard")
        .on_navigation(|_, url| {
            if app_url(url) {
                return true;
            }
            if matches!(url.scheme(), "http" | "https") {
                let _ = link::open_browser(url.as_str());
            }
            false
        })
        .build()
}

pub fn run() {
    let dirs = Dirs::detect();
    let wallet = wallet::Wallet::new(&dirs);
    let app = App {
        link: link::Link::default(),
        wallet,
        dirs,
        node: Mutex::new(None),
        node_error: Mutex::new(None),
        starting: Mutex::new(false),
        portmap: Mutex::new(None),
        renewing: AtomicBool::new(false),
        miner: Mutex::new(None),
    };
    let remote = crate::remote::Remote::new(Some(app.dirs.data.join("remote-node.txt")), Some(app.dirs.data.join("nodes.json")));
    // The current cycle of the verified chain is replayed in the background (lcverify.rs) with the app's ROM, once there is one.
    let (bundle, data) = (app.dirs.bundle.clone(), app.dirs.data.clone());
    remote.net.enable_replay(std::sync::Arc::new(move || rom::find(None, &bundle, &data).ok().and_then(|p| std::fs::read(p).ok())));
    let proxy = crate::start_net(&remote).expect("the wallet's loopback daemon could not start");
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(nav_guard())
        .manage(app)
        .manage(remote)
        .manage(proxy)
        .setup(|t| {
            // The node starts with the app when the ROM is there (the UI asks for it otherwise, then calls node_start).
            // SIGTERM / SIGINT / console close: leave through the normal exit path, which stops the miner and the node.
            let hx = t.handle().clone();
            let _ = ctrlc::set_handler(move || hx.exit(0));
            // The window (tauri.conf.json, `create: false`): built here so that in portable mode the webview keeps its
            // storage (localStorage, IndexedDB: the page's settings) in data/ as well.
            if let Some(cfg) = t.config().app.windows.iter().find(|w| w.label == "main").cloned() {
                let mut b = tauri::WebviewWindowBuilder::from_config(t.handle(), &cfg)?;
                let st = t.state::<App>();
                if st.dirs.portable {
                    let _ = std::fs::create_dir_all(&st.dirs.data);
                    b = b.data_directory(st.dirs.data.join("webview"));
                }
                b.build()?;
            }
            wallet::spawn_idle_lock(t.handle().clone());
            // The chain tip: followed once in Rust (the app's own node polled here, the light-client network's push or poll in
            // lcnet.rs) and told to the page as `tip` events; the verifier and the miner read the same hub.
            {
                let hub = t.state::<crate::remote::Remote>().net.tip_hub();
                crate::lctip::spawn_emitter(t.handle().clone(), hub.clone());
                let hh = t.handle().clone();
                crate::lctip::spawn_local_poll(hub, move || hh.state::<App>().node_rpc_port());
            }
            let h = t.handle().clone();
            std::thread::spawn(move || {
                let st = h.state::<App>();
                if load_settings(&st.dirs).auto_start && rom::find(None, &st.dirs.bundle, &st.dirs.data).is_ok() {
                    if let Err(e) = start_node(&st) {
                        eprintln!("node: {}", e.message);
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![host_info, rom_set, rom_pick, rom_bytes, node_start, node_stop, node_status, node_settings, node_settings_set, node_port_detect, node_log, node_rpc, crate::remote::remote_rpc, crate::remote::remote_node_get, crate::remote::net_status, crate::lctip::tip_get, remote_node_set, wallet::wallet_state, wallet::wallet_rpc, wallet::wallet_settings, wallet::wallet_activity, wallet::wallet_pin_enable, wallet::wallet_pin_open, wallet::wallet_pin_disable, link::link_start, link::link_status, link::link_cancel, link::link_finish, miner_start, miner_stop, miner_status, miner_viz_on, miner_viz_mail])
        .build(tauri::generate_context!())
        .expect("error while building the SM64Coin app")
        .run(|h, ev| {
            if let tauri::RunEvent::Exit = ev {
                // Reverse order: miner first, then the node (its stop waits for a clean database close).
                let st = h.state::<App>();
                st.wallet.shutdown();
                if let Some(mut m) = st.miner.lock().unwrap().take() {
                    m.stop();
                }
                if let Some(mut n) = st.node.lock().unwrap().take() {
                    n.stop();
                }
            }
        });
}

#[cfg(test)]
mod tests {
    #[test]
    fn portable_data_dir() {
        use super::data_dir;
        use std::path::Path;
        let root = std::env::temp_dir().join(format!("sm64coin-portable-test-{}", std::process::id()));
        let exe = root.join("app");
        std::fs::create_dir_all(&exe).unwrap();
        let home = Path::new("/home/someone");
        // Installed: the user's data dir.
        assert_eq!(data_dir(None, &exe, home), (home.join(".sm64coin"), false));
        // A folder named `portable` is not the marker.
        std::fs::create_dir_all(exe.join("portable")).unwrap();
        assert_eq!(data_dir(None, &exe, home), (home.join(".sm64coin"), false));
        std::fs::remove_dir(exe.join("portable")).unwrap();
        // The marker file: data/ next to the program.
        std::fs::write(exe.join("portable"), b"").unwrap();
        assert_eq!(data_dir(None, &exe, home), (exe.join("data"), true));
        // SM64COIN_DATA_DIR wins over both (an empty value does not count).
        assert_eq!(data_dir(Some("/srv/x".into()), &exe, home), ("/srv/x".into(), false));
        assert_eq!(data_dir(Some("".into()), &exe, home), (exe.join("data"), true));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn only_app_pages() {
        let ok = |u: &str| super::app_url(&u.parse().unwrap());
        assert!(ok("tauri://localhost/wallet") && ok("http://tauri.localhost/") && ok("https://tauri.localhost/mine"));
        assert!(!ok("https://sm64coin.pages.dev/") && !ok("https://tauri.localhost.evil.example/") && !ok("file:///etc/passwd"));
    }
}
