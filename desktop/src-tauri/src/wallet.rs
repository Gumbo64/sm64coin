//! The wallet: the `sm64coin-wallet-rpc` sidecar (the same wallet2 as the Qt GUI) and the protections the GUI has.
//!
//! - The sidecar listens on 127.0.0.1 only, on a random port, behind a random per-launch `--rpc-login` (HTTP digest) that lives
//!   in this process's memory only (it reaches the sidecar through a 0600 config file that is deleted once it is up, so it is
//!   not in `ps` either). The webview never sees port or login: it calls the `wallet_rpc` command, which has its own method
//!   whitelist and forwards.
//! - The wallet directory is mode 0700.
//! - Like the GUI: the password opens a wallet; it closes after N idle minutes (the page reports user activity with
//!   `wallet_activity`); sending asks for the password again (setting, default on); showing the seed or keys always does.
//!   The check is done here against a salted hash kept in memory while the wallet is open, so a page cannot skip it.
//! - Passwords, seeds and keys are never logged. They travel in the RPC body only, never in arguments or files.
use crate::desktop::{App, Dirs};
use crate::{AppError, Res};
use md5::{Digest as _, Md5};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use sm64coin_launcher::proc::Proc;
use sm64coin_launcher::util::pick_port;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Close the wallet after this many idle minutes; 0 = never.
    pub auto_lock_minutes: u32,
    /// Ask for the password again before sending.
    pub password_on_send: bool,
    /// Show balances as ••• until clicked.
    pub hide_balance: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Settings { auto_lock_minutes: 10, password_on_send: true, hide_balance: false }
    }
}

struct Sidecar {
    proc: Proc,
    port: u16,
    user: String,
    pass: String,
    kdf_rounds: u32,
}

struct Open {
    name: String,
    salt: [u8; 16],
    hash: [u8; 32],
}

pub struct Wallet {
    side: Mutex<Option<Sidecar>>,
    open: Mutex<Option<Open>>,
    settings: Mutex<Settings>,
    last_activity: Mutex<Instant>,
    /// The daemon wallet-rpc talks to: `host:port` of the local node, or an https URL (the public node).
    daemon: Mutex<Option<String>>,
    /// One wallet operation at a time (the sidecar is single-wallet, and create/restart must not race calls).
    gate: Mutex<()>,
}

fn rand_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("the system random generator is unavailable");
    b
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn pw_hash(salt: &[u8; 16], pw: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(pw.as_bytes());
    h.finalize().into()
}
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

impl Wallet {
    pub fn new(dirs: &Dirs) -> Wallet {
        let settings = std::fs::read_to_string(dirs.data.join("wallet-settings.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Wallet { side: Mutex::new(None), open: Mutex::new(None), settings: Mutex::new(settings), last_activity: Mutex::new(Instant::now()), daemon: Mutex::new(None), gate: Mutex::new(()) }
    }

    /// Stops the sidecar (on exit): the open wallet is stored by wallet-rpc itself when it gets SIGTERM.
    pub fn shutdown(&self) {
        *self.open.lock().unwrap() = None;
        if let Some(mut s) = self.side.lock().unwrap().take() {
            s.proc.stop(15);
        }
    }
}

fn wallets_dir(d: &Dirs) -> Res<PathBuf> {
    let p = d.data.join("wallets");
    std::fs::create_dir_all(&p).map_err(|e| AppError::new("wallet_dir", format!("cannot create {}: {e}", p.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).map_err(|e| AppError::new("wallet_dir", format!("cannot protect {}: {e}", p.display())))?;
    }
    Ok(p)
}

/// A wallet name is a plain file name: letters, digits, `-` and `_`.
fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 40 && n.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn list_wallets(d: &Dirs) -> Vec<String> {
    let mut v: Vec<String> = wallets_dir(d)
        .ok()
        .and_then(|p| std::fs::read_dir(p).ok())
        .map(|it| it.filter_map(|e| e.ok()).filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".keys").map(String::from))).filter(|n| valid_name(n)).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn default_daemon(app: &App) -> String {
    let port = app.node_rpc_port().unwrap_or_else(|| crate::desktop::port_env("SM64COIN_RPC_PORT", 16481));
    format!("127.0.0.1:{port}")
}

/// Starts the sidecar with `kdf_rounds` (a process-wide wallet2 option for wallets made by it) unless one with it runs.
fn ensure_sidecar(app: &App, want_kdf: Option<u32>) -> Res<()> {
    let kdf_rounds = want_kdf.unwrap_or(1);
    let w = &app.wallet;
    let mut side = w.side.lock().unwrap();
    if let Some(s) = side.as_mut() {
        if s.proc.alive() && want_kdf.is_none_or(|k| k == s.kdf_rounds) {
            return Ok(());
        }
        if w.open.lock().unwrap().is_some() && s.proc.alive() {
            return Err(AppError::new("wallet_busy", "close the open wallet first"));
        }
        s.proc.stop(15);
        *side = None;
    }
    let bin = app.dirs.tool("sm64coin-wallet-rpc");
    if !bin.is_file() {
        return Err(AppError::new("wallet_binary_missing", format!("{} is missing", bin.display())));
    }
    let dir = wallets_dir(&app.dirs)?;
    let logs = app.dirs.data.join("logs");
    let _ = std::fs::create_dir_all(&logs);
    let port = pick_port(20000 + (u16::from_le_bytes(rand_bytes::<2>()) % 30000), "127.0.0.1", &[]);
    let (user, pass) = (hex(&rand_bytes::<8>()), hex(&rand_bytes::<24>()));
    let daemon = w.daemon.lock().unwrap().clone().unwrap_or_else(|| default_daemon(app));
    let local = daemon.starts_with("127.0.0.1:");

    // The login goes in a config file readable by this user only, deleted as soon as the sidecar has read it.
    let cfg = app.dirs.data.join(format!("wallet-rpc-{}.conf", hex(&rand_bytes::<4>())));
    write_private(&cfg, &format!("rpc-login={user}:{pass}\n")).map_err(|e| AppError::new("wallet_start", format!("cannot write the sidecar's config: {e}")))?;
    let mut cmd = Command::new(&bin);
    cmd.arg("--config-file").arg(&cfg).arg("--wallet-dir").arg(&dir).arg("--rpc-bind-ip").arg("127.0.0.1").arg("--rpc-bind-port").arg(port.to_string());
    cmd.arg("--daemon-address").arg(&daemon).arg(if local { "--trusted-daemon" } else { "--untrusted-daemon" });
    cmd.arg("--kdf-rounds").arg(kdf_rounds.to_string()).arg("--non-interactive").arg("--no-dns").arg("--log-file").arg(logs.join("wallet-rpc-daemon.log"));
    let spawned = Proc::spawn("wallet-rpc", cmd, logs.join("wallet-rpc.out"));
    let mut proc = match spawned {
        Ok(p) => p,
        Err(e) => {
            let _ = std::fs::remove_file(&cfg);
            return Err(AppError::new("wallet_start", e));
        }
    };
    // Wait until it answers (with the login), then delete the config file.
    let t = Instant::now();
    let ready = loop {
        if !proc.alive() {
            break Err(format!("wallet-rpc exited: {}", proc.tail()));
        }
        if digest_post(port, &user, &pass, &json!({"jsonrpc":"2.0","id":0,"method":"get_version"}).to_string(), Duration::from_secs(3)).is_ok() {
            break Ok(());
        }
        if t.elapsed() > Duration::from_secs(30) {
            break Err("wallet-rpc did not answer within 30 s".to_string());
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let _ = std::fs::remove_file(&cfg);
    if let Err(e) = ready {
        proc.stop(3);
        return Err(AppError::new("wallet_start", e));
    }
    *side = Some(Sidecar { proc, port, user, pass, kdf_rounds });
    Ok(())
}

fn write_private(p: &std::path::Path, data: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(p)?.write_all(data.as_bytes())
}

// ---- HTTP digest (RFC 2617, MD5, qop=auth): what monero's RPC server asks for ----

fn md5hex(s: &str) -> String {
    hex(&Md5::digest(s.as_bytes()))
}

fn param(h: &str, key: &str) -> Option<String> {
    let k = format!("{key}=");
    let i = h.find(&k)?;
    let rest = &h[i + k.len()..];
    if let Some(r) = rest.strip_prefix('"') { r.find('"').map(|j| r[..j].to_string()) } else { Some(rest.split(',').next()?.trim().to_string()) }
}

/// POSTs `body` to the sidecar's /json_rpc. The first request carries no body (so no secret goes out unauthenticated).
fn digest_post(port: u16, user: &str, pass: &str, body: &str, timeout: Duration) -> Result<Value, String> {
    let url = format!("http://127.0.0.1:{port}/json_rpc");
    // The server ties its nonce to the connection: both requests go over one (an agent keeps it, once the 401's body is read).
    let agent = ureq::AgentBuilder::new().max_idle_connections(1).build();
    let challenge = match agent.post(&url).timeout(Duration::from_secs(5)).send_string("{}") {
        Err(ureq::Error::Status(401, r)) => {
            let c = r.header("www-authenticate").map(String::from).ok_or("no digest challenge")?;
            let _ = r.into_string();
            c
        }
        Ok(_) => return Err("the wallet RPC is not password protected".into()),
        Err(e) => return Err(e.to_string()),
    };
    let realm = param(&challenge, "realm").ok_or("bad challenge")?;
    let nonce = param(&challenge, "nonce").ok_or("bad challenge")?;
    let opaque = param(&challenge, "opaque");
    let cnonce = hex(&rand_bytes::<8>());
    let nc = "00000001";
    let ha1 = md5hex(&format!("{user}:{realm}:{pass}"));
    let ha2 = md5hex("POST:/json_rpc");
    let resp = md5hex(&format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}"));
    let mut auth = format!("Digest username=\"{user}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"/json_rpc\", algorithm=MD5, response=\"{resp}\", qop=auth, nc={nc}, cnonce=\"{cnonce}\"");
    if let Some(o) = opaque {
        auth.push_str(&format!(", opaque=\"{o}\""));
    }
    let r = agent.post(&url).timeout(timeout).set("Authorization", &auth).set("Content-Type", "application/json").send_string(body);
    match r {
        Ok(r) => serde_json::from_reader::<_, Value>(r.into_reader()).map_err(|e| e.to_string()),
        Err(ureq::Error::Status(c, _)) => Err(format!("wallet RPC answered HTTP {c}")),
        Err(e) => Err(e.to_string()),
    }
}

// ---- the commands ----

#[derive(Serialize)]
pub struct WalletInfo {
    /// The sidecar binary is there.
    available: bool,
    running: bool,
    /// Wallet files in the wallet directory.
    wallets: Vec<String>,
    /// The open wallet.
    open: Option<String>,
    settings: Settings,
    /// This user's wallet directory.
    dir: String,
}

fn info(app: &App) -> WalletInfo {
    let w = &app.wallet;
    // A start in progress holds the lock for a while: report it as running rather than wait.
    let running = w.side.try_lock().map(|mut g| g.as_mut().is_some_and(|s| s.proc.alive())).unwrap_or(true);
    if !running {
        *w.open.lock().unwrap() = None; // the sidecar died: nothing is open any more
    }
    WalletInfo {
        available: app.dirs.tool("sm64coin-wallet-rpc").is_file(),
        running,
        wallets: list_wallets(&app.dirs),
        open: w.open.lock().unwrap().as_ref().map(|o| o.name.clone()),
        settings: w.settings.lock().unwrap().clone(),
        dir: app.dirs.data.join("wallets").display().to_string(),
    }
}

#[tauri::command]
pub fn wallet_state(app: State<'_, App>) -> Res<WalletInfo> {
    Ok(info(&app))
}

/// The page reports that the user is there (clicks, keys): it restarts the idle timer.
#[tauri::command]
pub fn wallet_activity(app: State<'_, App>) -> Res<()> {
    *app.wallet.last_activity.lock().unwrap() = Instant::now();
    Ok(())
}

#[tauri::command]
pub async fn wallet_settings(app: AppHandle, settings: Settings, password: Option<String>) -> Res<Settings> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<App>();
        let w = &st.wallet;
        let old = w.settings.lock().unwrap().clone();
        // Turning a protection off needs the password of the open wallet (a page cannot weaken them by itself).
        let weaker = (old.password_on_send && !settings.password_on_send) || (old.auto_lock_minutes != 0 && (settings.auto_lock_minutes == 0 || settings.auto_lock_minutes > old.auto_lock_minutes));
        if weaker {
            check_password(&st, password.as_deref())?;
        }
        let mut s = settings;
        s.auto_lock_minutes = s.auto_lock_minutes.min(24 * 60);
        let json = serde_json::to_string_pretty(&s).map_err(|e| AppError::new("internal", e.to_string()))?;
        std::fs::write(st.dirs.data.join("wallet-settings.json"), json).map_err(|e| AppError::new("wallet_settings", e.to_string()))?;
        *w.settings.lock().unwrap() = s.clone();
        Ok(s)
    })
    .await
    .map_err(|e| AppError::new("internal", e.to_string()))?
}

/// Verifies `password` against the open wallet (the salted hash kept at open time).
fn check_password(app: &App, password: Option<&str>) -> Res<()> {
    let g = app.wallet.open.lock().unwrap();
    let o = g.as_ref().ok_or_else(|| AppError::new("wallet_not_open", "no wallet is open"))?;
    let pw = password.ok_or_else(|| AppError::new("password_required", "enter the wallet password"))?;
    if ct_eq(&pw_hash(&o.salt, pw), &o.hash) { Ok(()) } else { Err(AppError::new("wrong_password", "wrong password")) }
}

fn remember(app: &App, name: &str, password: &str) {
    let salt = rand_bytes::<16>();
    *app.wallet.open.lock().unwrap() = Some(Open { name: name.to_string(), hash: pw_hash(&salt, password), salt });
    *app.wallet.last_activity.lock().unwrap() = Instant::now();
}

fn str_param<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(|v| v.as_str())
}

fn call(app: &App, method: &str, params: Value, timeout: Duration) -> Res<Value> {
    let (port, user, pass) = {
        let mut g = app.wallet.side.lock().unwrap();
        let s = g.as_mut().ok_or_else(|| AppError::new("wallet_not_running", "the wallet is not running"))?;
        if !s.proc.alive() {
            return Err(AppError::new("wallet_not_running", "the wallet is not running"));
        }
        (s.port, s.user.clone(), s.pass.clone())
    };
    let mut params = params;
    let body = Zeroizing::new(json!({"jsonrpc": "2.0", "id": "0", "method": method, "params": &params}).to_string());
    scrub(&mut params);
    digest_post(port, &user, &pass, &body, timeout).map_err(|e| AppError::new("wallet_rpc", e))
}

/// Wipes the secrets of a request (seed words, passwords) once it is serialized.
fn scrub(v: &mut Value) {
    if let Some(o) = v.as_object_mut() {
        for (k, x) in o.iter_mut() {
            if matches!(k.as_str(), "seed" | "password" | "old_password" | "new_password" | "seed_offset") {
                if let Value::String(s) = x {
                    s.zeroize();
                }
            }
        }
    }
}

/// Methods the page may call. Everything else (export_*, sweep, set_attribute, make_integrated_address, ...) is refused.
const ALLOWED: &[&str] = &[
    "get_version", "get_height", "get_balance", "get_address", "get_transfers", "get_transfer_by_txid", "validate_address", "get_languages",
    "store", "refresh", "rescan_blockchain", "relay_tx", "transfer", "query_key", "set_daemon", "get_address_book",
    "create_wallet", "open_wallet", "restore_deterministic_wallet", "close_wallet",
];

pub fn lock_wallet(app: &App) {
    if app.wallet.open.lock().unwrap().is_none() {
        return;
    }
    let _ = call(app, "store", json!({}), Duration::from_secs(60));
    let _ = call(app, "close_wallet", json!({}), Duration::from_secs(60));
    *app.wallet.open.lock().unwrap() = None;
}

#[tauri::command]
pub async fn wallet_rpc(app: AppHandle, method: String, params: Option<Value>) -> Res<Value> {
    tauri::async_runtime::spawn_blocking(move || wallet_call(&app, &method, params.unwrap_or_else(|| json!({}))))
        .await
        .map_err(|e| AppError::new("internal", e.to_string()))?
}

fn wallet_call(handle: &AppHandle, method: &str, mut p: Value) -> Res<Value> {
    if !ALLOWED.contains(&method) {
        return Err(AppError::new("forbidden", format!("wallet method {method} is not available")));
    }
    let st = handle.state::<App>();
    let app: &App = &st;
    let _gate = app.wallet.gate.lock().unwrap();
    // `_password` is for this layer only: it is removed before forwarding.
    let password = p.as_object_mut().and_then(|o| o.remove("_password")).and_then(|v| v.as_str().map(String::from));
    let kdf = p.as_object_mut().and_then(|o| o.remove("_kdf_rounds")).and_then(|v| v.as_u64()).map(|v| v.clamp(1, 1_000_000) as u32);
    let slow = Duration::from_secs(600);
    match method {
        "create_wallet" | "restore_deterministic_wallet" | "open_wallet" => {
            let name = str_param(&p, "filename").unwrap_or("").to_string();
            if !valid_name(&name) {
                return Err(AppError::new("bad_name", "a wallet name is letters, digits, - and _ (up to 40)"));
            }
            let pw = Zeroizing::new(str_param(&p, "password").unwrap_or("").to_string());
            if method != "open_wallet" && pw.len() < 8 {
                return Err(AppError::new("weak_password", "use a password of at least 8 characters"));
            }
            if app.wallet.open.lock().unwrap().is_some() {
                lock_wallet(app);
            }
            ensure_sidecar(app, if method == "open_wallet" { None } else { Some(kdf.unwrap_or(1)) })?;
            let r = call(app, method, p, slow)?;
            if r.get("error").is_none() {
                remember(app, &name, &pw);
            }
            Ok(r)
        }
        "close_wallet" => {
            if app.wallet.open.lock().unwrap().is_none() {
                return Ok(json!({"result": {}}));
            }
            let _ = call(app, "store", json!({}), slow);
            let r = call(app, "close_wallet", json!({}), slow)?;
            *app.wallet.open.lock().unwrap() = None;
            Ok(r)
        }
        _ => {
            // Everything below needs an open wallet (except the harmless ones).
            let needs_wallet = !matches!(method, "get_version" | "get_languages" | "validate_address" | "set_daemon");
            if needs_wallet && app.wallet.open.lock().unwrap().is_none() {
                return Err(AppError::new("wallet_not_open", "no wallet is open"));
            }
            match method {
                "query_key" => check_password(app, password.as_deref())?, // seed, spend key, view key: always the password
                "transfer" => {
                    if app.wallet.settings.lock().unwrap().password_on_send {
                        check_password(app, password.as_deref())?;
                    }
                    amounts_from_strings(&mut p);
                    // Review first, send second: the page may only build the transaction, then `relay_tx` it.
                    if let Some(o) = p.as_object_mut() {
                        o.insert("do_not_relay".into(), json!(true));
                        o.insert("get_tx_metadata".into(), json!(true));
                    }
                }
                "set_daemon" => {
                    // Only this app's own node (its RPC port, trusted) or an allowed https node (the public one, or the one the
                    // user confirmed on the Network page; untrusted): never another local port or any other remote.
                    let a = str_param(&p, "address").unwrap_or("").to_string();
                    let local = a == default_daemon(app);
                    let remote = handle.state::<crate::remote::Remote>().allowed(&a);
                    if !local && remote.is_none() {
                        return Err(AppError::new("forbidden", "the wallet's node must be this app's own node, the public node or the one saved on the Network page"));
                    }
                    let a = remote.unwrap_or(a);
                    *app.wallet.daemon.lock().unwrap() = Some(a.clone());
                    p = json!({"address": a, "trusted": local, "ssl_support": if local { "disabled" } else { "enabled" }});
                    if !app.wallet.side.lock().unwrap().as_mut().is_some_and(|s| s.proc.alive()) {
                        return Ok(json!({"result": {}})); // applied when the sidecar starts
                    }
                }
                _ => {}
            }
            ensure_sidecar(app, None)?;
            let mut r = call(app, method, p, if matches!(method, "refresh" | "rescan_blockchain") { slow } else { Duration::from_secs(120) })?;
            amounts_to_strings(&mut r);
            Ok(r)
        }
    }
}

/// Amounts are atomic units (12 decimals): above 2^53 (about 9000 coins) a JS number loses digits, so the answer carries them as
/// strings (the page uses BigInt), and a transfer's amounts may come as strings.
const AMOUNT_KEYS: &[&str] = &["amount", "balance", "unlocked_balance", "fee", "blocks_to_unlock", "time_to_unlock"];
fn amounts_to_strings(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if AMOUNT_KEYS.contains(&k.as_str()) && x.is_u64() {
                    *x = Value::String(x.as_u64().unwrap().to_string());
                } else {
                    amounts_to_strings(x);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(amounts_to_strings),
        _ => {}
    }
}
fn amounts_from_strings(p: &mut Value) {
    if let Some(d) = p.get_mut("destinations").and_then(|d| d.as_array_mut()) {
        for x in d {
            if let Some(a) = x.get("amount").and_then(|a| a.as_str()).and_then(|a| a.parse::<u64>().ok()) {
                x["amount"] = json!(a);
            }
        }
    }
}

/// Restores a wallet from seed words (also what "Use my website passkey" ends in). With `expect`, the restored wallet must have
/// that primary address; otherwise it is closed and its new files are removed (code `address_mismatch`).
pub fn restore_from_seed(app: &AppHandle, name: &str, password: &str, seed: &str, height: u64, kdf_rounds: Option<u32>, expect: Option<&str>) -> Res<Value> {
    let r = wallet_call(app, "restore_deterministic_wallet", json!({"filename": name, "password": password, "seed": seed, "restore_height": height, "seed_offset": "", "_kdf_rounds": kdf_rounds.unwrap_or(1)}))?;
    if let Some(e) = r.get("error") {
        return Err(AppError::new("wallet_rpc", e.get("message").and_then(|m| m.as_str()).unwrap_or("restore failed")));
    }
    if let Some(want) = expect {
        let got = r.get("result").and_then(|x| x.get("address")).and_then(|a| a.as_str()).unwrap_or("");
        if got != want {
            let st = app.state::<App>();
            {
                let _gate = st.wallet.gate.lock().unwrap();
                lock_wallet(&st);
            }
            if let Ok(dir) = wallets_dir(&st.dirs) {
                let _ = std::fs::remove_file(dir.join(name));
                let _ = std::fs::remove_file(dir.join(format!("{name}.keys")));
                let _ = std::fs::remove_file(dir.join(format!("{name}.address.txt")));
            }
            return Err(AppError::new("address_mismatch", "the received seed words do not belong to the address that was shown; nothing was imported"));
        }
    }
    Ok(r)
}

/// Closes the wallet after `auto_lock_minutes` without user activity (the GUI's "lock after N minutes").
pub fn spawn_idle_lock(handle: AppHandle) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(5));
            let st = handle.state::<App>();
            let (minutes, idle) = (st.wallet.settings.lock().unwrap().auto_lock_minutes, st.wallet.last_activity.lock().unwrap().elapsed());
            if minutes > 0 && idle >= Duration::from_secs(minutes as u64 * 60) && st.wallet.open.lock().unwrap().is_some() {
                let _gate = st.wallet.gate.lock().unwrap();
                // A call may have held the gate for a while: lock only if the user is still away.
                if st.wallet.last_activity.lock().unwrap().elapsed() < Duration::from_secs(minutes as u64 * 60) {
                    continue;
                }
                lock_wallet(&st);
                let _ = handle.emit("wallet-locked", "idle");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_against_a_running_sidecar() {
        // Needs a wallet-rpc started by hand: WALLET_TEST_PORT, WALLET_TEST_LOGIN=user:pass. Skipped otherwise.
        let (Ok(port), Ok(login)) = (std::env::var("WALLET_TEST_PORT"), std::env::var("WALLET_TEST_LOGIN")) else { return };
        let (u, p) = login.split_once(':').unwrap();
        let r = digest_post(port.parse().unwrap(), u, p, r#"{"jsonrpc":"2.0","id":"0","method":"get_version"}"#, Duration::from_secs(5)).unwrap();
        assert!(r["result"]["version"].is_number(), "{r}");
        assert!(digest_post(port.parse().unwrap(), u, "wrong", r#"{"jsonrpc":"2.0","id":"0","method":"get_version"}"#, Duration::from_secs(5)).is_err());
    }
}
