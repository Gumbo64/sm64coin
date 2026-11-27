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
//! - Quick-unlock (optional, off by default): a PIN seals the wallet password (walletpin.rs); the PIN opens the wallet from here
//!   (the password never reaches the page) and is accepted wherever the password is asked (send, seed and keys, weakening a setting).
//! - Passwords, PINs, seeds and keys are never logged. They travel in the RPC body only, never in arguments or files.
use crate::desktop::{App, Dirs};
use crate::walletpin::{self, Attempt};
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

/// The KDF rounds a wallet was made with (wallet2 needs the same value to open it, and the sidecar takes it once, at start):
/// kept next to the wallet as `<name>.kdf`, a plain number (not secret). A wallet without one was made with the default, 1.
fn kdf_path(dir: &std::path::Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.kdf"))
}

fn read_kdf(dir: &std::path::Path, name: &str) -> Option<u32> {
    std::fs::read_to_string(kdf_path(dir, name)).ok().and_then(|s| s.trim().parse::<u32>().ok()).filter(|k| (1..=1_000_000).contains(k))
}

fn write_kdf(dir: &std::path::Path, name: &str, rounds: u32) -> std::io::Result<()> {
    std::fs::write(kdf_path(dir, name), format!("{rounds}\n"))
}

/// The rounds to start the sidecar with for `method` on wallet `name`: create/restore take the page's choice (default 1);
/// open takes the page's explicit value, else the one stored with the wallet, else 1.
fn kdf_for(method: &str, asked: Option<u32>, stored: Option<u32>) -> u32 {
    if method == "open_wallet" { asked.or(stored).unwrap_or(1) } else { asked.unwrap_or(1) }
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
    // Only this app's own node is trusted (the loopback proxy of walletproxy.rs, also 127.0.0.1, stands for remote nodes).
    let local = daemon == default_daemon(app);

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
    /// Wallets with a PIN set, and the wrong tries each still has.
    pins: Vec<PinInfo>,
}

#[derive(Serialize)]
struct PinInfo {
    name: String,
    left: u32,
    /// The wallet's password was made by the PIN setup (turning the PIN off asks for a new one).
    generated: bool,
}

fn pins_dir(d: &Dirs) -> Res<PathBuf> {
    let p = d.data.join("wallet-pins");
    std::fs::create_dir_all(&p).map_err(|e| AppError::new("wallet_dir", format!("cannot create {}: {e}", p.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700));
    }
    Ok(p)
}
fn pin_path(d: &Dirs, name: &str) -> Res<PathBuf> {
    if !valid_name(name) {
        return Err(AppError::new("bad_name", "a wallet name is letters, digits, - and _ (up to 40)"));
    }
    Ok(pins_dir(d)?.join(format!("{name}.pin")))
}
fn list_pins(d: &Dirs, wallets: &[String]) -> Vec<PinInfo> {
    let Ok(dir) = pins_dir(d) else { return vec![] };
    wallets
        .iter()
        .filter_map(|n| {
            let r = walletpin::load(&dir.join(format!("{n}.pin")))?;
            Some(PinInfo { name: n.clone(), left: r.tries.clamp(1, 100).saturating_sub(r.fails), generated: r.generated })
        })
        .collect()
}

fn info(app: &App) -> WalletInfo {
    let w = &app.wallet;
    // A start in progress holds the lock for a while: report it as running rather than wait.
    let running = w.side.try_lock().map(|mut g| g.as_mut().is_some_and(|s| s.proc.alive())).unwrap_or(true);
    if !running {
        *w.open.lock().unwrap() = None; // the sidecar died: nothing is open any more
    }
    let wallets = list_wallets(&app.dirs);
    WalletInfo {
        available: app.dirs.tool("sm64coin-wallet-rpc").is_file(),
        running,
        pins: list_pins(&app.dirs, &wallets),
        wallets,
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
pub async fn wallet_settings(app: AppHandle, settings: Settings, password: Option<String>, pin: Option<String>) -> Res<Settings> {
    let (password, pin) = (password.map(Zeroizing::new), pin.map(Zeroizing::new));
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<App>();
        let w = &st.wallet;
        let old = w.settings.lock().unwrap().clone();
        // Turning a protection off needs the password of the open wallet (a page cannot weaken them by itself).
        let weaker = (old.password_on_send && !settings.password_on_send) || (old.auto_lock_minutes != 0 && (settings.auto_lock_minutes == 0 || settings.auto_lock_minutes > old.auto_lock_minutes));
        if weaker {
            gate(&st, password.as_ref().map(|p| p.as_str()), pin.as_ref().map(|p| p.as_str()))?;
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

/// The password gate with quick-unlock: the password if it is right, else (when the open wallet has a PIN and one was given)
/// the PIN, which counts as an attempt and opens the sealed password. Returns the verified password.
fn gate(app: &App, password: Option<&str>, pin: Option<&str>) -> Res<Zeroizing<String>> {
    if check_password(app, password).is_ok() {
        return Ok(Zeroizing::new(password.unwrap_or("").to_string()));
    }
    let name = app.wallet.open.lock().unwrap().as_ref().map(|o| o.name.clone()).ok_or_else(|| AppError::new("wallet_not_open", "no wallet is open"))?;
    if let (Some(pin), Ok(path)) = (pin, pin_path(&app.dirs, &name)) {
        if path.is_file() {
            let secret = pin_try(&path, &name, pin)?;
            // the sealed password must still be the wallet's (it is, unless the wallet was changed by hand)
            check_password(app, Some(secret.as_str()))?;
            return Ok(secret);
        }
    }
    if password.is_none() {
        return Err(AppError::new("password_required", "enter the wallet password"));
    }
    Err(AppError::new("wrong_password", "wrong password"))
}

/// One PIN try, as an error the page can show: `wrong_pin` (tries left in the message) or `pin_wiped`.
fn pin_try(path: &std::path::Path, name: &str, pin: &str) -> Res<Zeroizing<String>> {
    match walletpin::attempt(path, pin, name).map_err(|e| AppError::new("wallet_pin", e))? {
        Attempt::Ok(s) => Ok(s),
        Attempt::Wrong { left } => Err(AppError::new("wrong_pin", format!("Wrong PIN: {left} {} left.", if left == 1 { "try" } else { "tries" }))),
        Attempt::Wiped => Err(AppError::new("pin_wiped", "The PIN is gone (too many wrong tries): use the wallet password (if the PIN setup made it, restore the wallet from its seed words).")),
    }
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
    "store", "refresh", "rescan_blockchain", "relay_tx", "transfer", "query_key", "set_daemon",
    "get_address_book", "add_address_book", "edit_address_book", "delete_address_book",
    "create_wallet", "open_wallet", "restore_deterministic_wallet", "close_wallet", "change_wallet_password",
];

pub fn lock_wallet(app: &App) {
    if app.wallet.open.lock().unwrap().is_none() {
        return;
    }
    let _ = call(app, "store", json!({}), Duration::from_secs(60));
    let _ = call(app, "close_wallet", json!({}), Duration::from_secs(60));
    *app.wallet.open.lock().unwrap() = None;
}

/// `validate_address` answered "valid, on this chain's network".
fn address_ok(v: &Value) -> bool {
    let r = v.get("result").unwrap_or(v);
    r.get("valid").and_then(|x| x.as_bool()) == Some(true) && r.get("nettype").and_then(|x| x.as_str()).is_none_or(|n| n == "mainnet")
}

/// Invisible characters that reorder or hide text (bidi overrides and isolates, zero-width ones): a label must not use them to
/// look like another one.
fn is_hidden_format(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
}

/// Address book entries: a label of up to 120 printable characters.
fn check_book_params(p: &Value) -> Res<()> {
    if let Some(d) = str_param(p, "description") {
        if d.chars().count() > 120 || d.chars().any(|c| c.is_control() || is_hidden_format(c)) {
            return Err(AppError::new("bad_label", "a label is up to 120 characters, no control characters"));
        }
    }
    Ok(())
}

/// The address an address-book call saves, to be validated first: always for an add; for an edit only when it sets the address
/// (a label-only edit sends `address: ""` with `set_address: false`).
fn book_address(method: &str, p: &Value) -> Option<String> {
    let sets = method == "add_address_book" || p.get("set_address").and_then(|v| v.as_bool()) == Some(true);
    if !sets {
        return None;
    }
    Some(str_param(p, "address").unwrap_or("").to_string())
}

fn random_password() -> Zeroizing<String> {
    // 24 random bytes as 48 hex characters: far above the 8-character minimum, nothing to type or remember
    Zeroizing::new(hex(&rand_bytes::<24>()))
}

/// Turns quick-unlock on for the open wallet: `pin` (4-8 digits) seals the wallet password. A wallet without a password gets a
/// random one (so the wallet file is encrypted from now on; the page is told `generated`); otherwise `password` (the current one)
/// is sealed. The password never goes back to the page.
#[tauri::command]
pub async fn wallet_pin_enable(app: AppHandle, pin: String, password: Option<String>) -> Res<Value> {
    let (pin, password) = (Zeroizing::new(pin), password.map(Zeroizing::new));
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<App>();
        let app: &App = &st;
        let _gate = app.wallet.gate.lock().unwrap();
        if !walletpin::valid_pin(&pin) {
            return Err(AppError::new("bad_pin", "a PIN is 4 to 8 digits"));
        }
        let name = app.wallet.open.lock().unwrap().as_ref().map(|o| o.name.clone()).ok_or_else(|| AppError::new("wallet_not_open", "no wallet is open"))?;
        let path = pin_path(&app.dirs, &name)?;
        let old = password.unwrap_or_default();
        check_password(app, Some(old.as_str()))?;
        let generated = old.is_empty();
        let secret = if generated { random_password() } else { old.clone() };
        let rec = walletpin::wrap(&secret, &pin, &name, walletpin::PIN_ITER, walletpin::PIN_TRIES, generated).map_err(|e| AppError::new("wallet_pin", e))?;
        walletpin::save(&path, &rec).map_err(|e| AppError::new("wallet_pin", e))?;
        if generated {
            let r = call(app, "change_wallet_password", json!({"old_password": old.as_str(), "new_password": secret.as_str()}), Duration::from_secs(120));
            if !matches!(&r, Ok(v) if v.get("error").is_none()) {
                walletpin::remove(&path);
                return Err(AppError::new("wallet_rpc", "could not set the wallet's password"));
            }
            let _ = call(app, "store", json!({}), Duration::from_secs(60));
            remember(app, &name, &secret);
        }
        Ok(json!({"generated": generated}))
    })
    .await
    .map_err(|e| AppError::new("internal", e.to_string()))?
}

/// Opens wallet `name` with its PIN (a wrong one is counted; the fifth wipes the PIN).
#[tauri::command]
pub async fn wallet_pin_open(app: AppHandle, name: String, pin: String) -> Res<Value> {
    let pin = Zeroizing::new(pin);
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<App>();
        let path = pin_path(&st.dirs, &name)?;
        if !path.is_file() {
            return Err(AppError::new("pin_wiped", "This wallet has no PIN: use the wallet password."));
        }
        let secret = pin_try(&path, &name, &pin)?;
        let r = wallet_call(&app, "open_wallet", json!({"filename": name, "password": secret.as_str()}))?;
        Ok(r)
    })
    .await
    .map_err(|e| AppError::new("internal", e.to_string()))?
}

/// Turns quick-unlock off for the open wallet. `password` or `pin` proves it is the user. If the PIN setup had made the
/// wallet's password (`generated`), `new_password` becomes the password ('' = none, like a one-click wallet); otherwise the
/// user's own password simply stays.
#[tauri::command]
pub async fn wallet_pin_disable(app: AppHandle, password: Option<String>, pin: Option<String>, new_password: Option<String>) -> Res<Value> {
    let (password, pin) = (password.map(Zeroizing::new), pin.map(Zeroizing::new));
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<App>();
        let app: &App = &st;
        let _gate = app.wallet.gate.lock().unwrap();
        let name = app.wallet.open.lock().unwrap().as_ref().map(|o| o.name.clone()).ok_or_else(|| AppError::new("wallet_not_open", "no wallet is open"))?;
        let path = pin_path(&app.dirs, &name)?;
        let rec = walletpin::load(&path);
        let old = gate(app, password.as_ref().map(|p| p.as_str()), pin.as_ref().map(|p| p.as_str()))?;
        let generated = rec.as_ref().is_some_and(|r| r.generated);
        if generated {
            let new = Zeroizing::new(new_password.unwrap_or_default());
            if !new.is_empty() && new.len() < 8 {
                return Err(AppError::new("weak_password", "a password is at least 8 characters (or leave it empty)"));
            }
            let r = call(app, "change_wallet_password", json!({"old_password": old.as_str(), "new_password": new.as_str()}), Duration::from_secs(120))?;
            if r.get("error").is_some() {
                return Err(AppError::new("wallet_rpc", "could not change the wallet's password"));
            }
            let _ = call(app, "store", json!({}), Duration::from_secs(60));
            remember(app, &name, &new);
        }
        walletpin::remove(&path);
        Ok(json!({"generated": generated}))
    })
    .await
    .map_err(|e| AppError::new("internal", e.to_string()))?
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
    let pin = p.as_object_mut().and_then(|o| o.remove("_pin")).and_then(|v| v.as_str().map(String::from)).map(Zeroizing::new);
    let kdf = p.as_object_mut().and_then(|o| o.remove("_kdf_rounds")).and_then(|v| v.as_u64()).map(|v| v.clamp(1, 1_000_000) as u32);
    let slow = Duration::from_secs(600);
    match method {
        "create_wallet" | "restore_deterministic_wallet" | "open_wallet" => {
            let name = str_param(&p, "filename").unwrap_or("").to_string();
            if !valid_name(&name) {
                return Err(AppError::new("bad_name", "a wallet name is letters, digits, - and _ (up to 40)"));
            }
            let pw = Zeroizing::new(str_param(&p, "password").unwrap_or("").to_string());
            if method != "open_wallet" && !pw.is_empty() && pw.len() < 8 {
                return Err(AppError::new("weak_password", "a password is at least 8 characters (or leave it empty)"));
            }
            if app.wallet.open.lock().unwrap().is_some() {
                lock_wallet(app);
            }
            let dir = wallets_dir(&app.dirs)?;
            let rounds = kdf_for(method, kdf, read_kdf(&dir, &name));
            ensure_sidecar(app, Some(rounds))?;
            let r = call(app, method, p, slow)?;
            if r.get("error").is_none() {
                if method != "open_wallet" {
                    // a new wallet under the name of an old one (deleted by hand): the old wallet's PIN must not stay
                    if let Ok(pp) = pin_path(&app.dirs, &name) {
                        walletpin::remove(&pp);
                    }
                }
                remember(app, &name, &pw);
                // Made here (or opened with an explicit value): remember the rounds, so the next start opens it.
                if method != "open_wallet" || kdf.is_some() {
                    if let Err(e) = write_kdf(&dir, &name, rounds) {
                        eprintln!("wallet: cannot save {name}.kdf: {e}");
                    }
                }
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
            let mut new_pw: Option<Zeroizing<String>> = None;
            match method {
                "query_key" => {
                    gate(app, password.as_deref(), pin.as_ref().map(|p| p.as_str()))?; // seed, spend key, view key: always the password (or the PIN)
                }
                "change_wallet_password" => {
                    // Set or change the open wallet's password (a wallet made without one has the empty password): the current one is
                    // checked here, the new one is empty-or-8+ like at creation.
                    let old = gate(app, password.as_deref(), pin.as_ref().map(|p| p.as_str()))?;
                    let new = Zeroizing::new(str_param(&p, "new_password").unwrap_or("").to_string());
                    if new.is_empty() || new.len() < 8 {
                        return Err(AppError::new("weak_password", "a password is at least 8 characters"));
                    }
                    if app.dirs.data.join("wallet-pins").join(format!("{}.pin", app.wallet.open.lock().unwrap().as_ref().map(|o| o.name.as_str()).unwrap_or(""))).is_file() {
                        // the PIN seals the old password: changing it by hand would strand the PIN (turn the PIN off first)
                        return Err(AppError::new("pin_set", "turn the PIN off before changing the password"));
                    }
                    p = json!({"old_password": old.as_str(), "new_password": new.as_str()});
                    new_pw = Some(new);
                }
                "transfer" => {
                    if app.wallet.settings.lock().unwrap().password_on_send {
                        gate(app, password.as_deref(), pin.as_ref().map(|p| p.as_str()))?;
                    }
                    amounts_from_strings(&mut p);
                    // Review first, send second: the page may only build the transaction, then `relay_tx` it.
                    if let Some(o) = p.as_object_mut() {
                        o.insert("do_not_relay".into(), json!(true));
                        o.insert("get_tx_metadata".into(), json!(true));
                    }
                }
                "add_address_book" | "edit_address_book" => {
                    check_book_params(&p)?;
                    if let Some(a) = book_address(method, &p) {
                        // wallet2 would also resolve an OpenAlias name here: only a plain address of this chain is saved
                        ensure_sidecar(app, None)?;
                        let v = call(app, "validate_address", json!({"address": a, "any_net_type": false}), Duration::from_secs(20))?;
                        if !address_ok(&v) {
                            return Err(AppError::new("bad_address", "this is not an SM64Coin address"));
                        }
                    }
                }
                "set_daemon" => {
                    // Only this app's own node (its RPC port, trusted) or an allowed https node (the public one, or the one the
                    // user confirmed on the Network page; untrusted): never another local port or any other remote.
                    let a = str_param(&p, "address").unwrap_or("").to_string();
                    let local = a == default_daemon(app);
                    // The loopback daemon of this app (fails over between the public nodes): plain http, untrusted.
                    let via_proxy = handle.state::<std::sync::Arc<crate::walletproxy::WalletProxy>>().addr() == a;
                    let remote = handle.state::<crate::remote::Remote>().allowed(&a);
                    if !local && !via_proxy && remote.is_none() {
                        return Err(AppError::new("forbidden", "the wallet's node must be this app's own node, the app's loopback daemon, the public node or the one saved on the Network page"));
                    }
                    let a = remote.unwrap_or(a);
                    *app.wallet.daemon.lock().unwrap() = Some(a.clone());
                    let plain = local || via_proxy;
                    p = json!({"address": a, "trusted": local, "ssl_support": if plain { "disabled" } else { "enabled" }});
                    if !app.wallet.side.lock().unwrap().as_mut().is_some_and(|s| s.proc.alive()) {
                        return Ok(json!({"result": {}})); // applied when the sidecar starts
                    }
                }
                _ => {}
            }
            ensure_sidecar(app, None)?;
            let mut r = call(app, method, p, if matches!(method, "refresh" | "rescan_blockchain") { slow } else { Duration::from_secs(120) })?;
            amounts_to_strings(&mut r);
            if let Some(new) = new_pw {
                if r.get("error").is_none() {
                    let name = app.wallet.open.lock().unwrap().as_ref().map(|o| o.name.clone());
                    if let Some(name) = name {
                        remember(app, &name, &new);
                    }
                }
            }
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
                let _ = std::fs::remove_file(kdf_path(&dir, name));
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
    fn kdf_rounds_are_kept_per_wallet() {
        let dir = std::env::temp_dir().join(format!("sm64coin-kdf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_kdf(&dir, "w1"), None);
        write_kdf(&dir, "w1", 64).unwrap();
        assert_eq!(read_kdf(&dir, "w1"), Some(64));
        assert_eq!(read_kdf(&dir, "w2"), None);
        std::fs::write(kdf_path(&dir, "w2"), "junk").unwrap();
        assert_eq!(read_kdf(&dir, "w2"), None);
        std::fs::write(kdf_path(&dir, "w2"), "0").unwrap();
        assert_eq!(read_kdf(&dir, "w2"), None);
        // open: the page's explicit value, else the stored one, else 1; create/restore: the page's, else 1.
        assert_eq!(kdf_for("open_wallet", None, Some(64)), 64);
        assert_eq!(kdf_for("open_wallet", Some(8), Some(64)), 8);
        assert_eq!(kdf_for("open_wallet", None, None), 1);
        assert_eq!(kdf_for("create_wallet", None, Some(64)), 1);
        assert_eq!(kdf_for("restore_deterministic_wallet", Some(32), None), 32);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn address_book_rules() {
        assert!(address_ok(&json!({"result": {"valid": true, "nettype": "mainnet", "integrated": false}})));
        assert!(address_ok(&json!({"valid": true})));
        assert!(!address_ok(&json!({"result": {"valid": false, "nettype": "mainnet"}})));
        assert!(!address_ok(&json!({"result": {"valid": true, "nettype": "stagenet"}})));
        assert!(!address_ok(&json!({"error": {"code": -1}})));
        assert!(check_book_params(&json!({"description": "Alice"})).is_ok());
        assert!(check_book_params(&json!({"description": "x".repeat(121)})).is_err());
        assert!(check_book_params(&json!({"description": "a\nb"})).is_err());
        assert!(check_book_params(&json!({"description": "Alice\u{202e}ecilA"})).is_err(), "a bidi override");
        assert!(check_book_params(&json!({"description": "Zoë 💰 東京"})).is_ok());
        // a label-only edit (what the page's Rename sends) has no address to check; an add or an address edit always has one
        assert_eq!(book_address("edit_address_book", &json!({"index": 0, "set_address": false, "address": "", "set_description": true, "description": "x"})), None);
        assert_eq!(book_address("edit_address_book", &json!({"index": 0, "set_address": true, "address": "SM6x"})).as_deref(), Some("SM6x"));
        assert_eq!(book_address("add_address_book", &json!({"description": "x"})).as_deref(), Some(""), "an add without an address is checked (and refused)");
        for m in ["get_address_book", "add_address_book", "edit_address_book", "delete_address_book"] {
            assert!(ALLOWED.contains(&m), "{m} is whitelisted");
        }
        // what stays refused
        for m in ["export_outputs", "sweep_all", "set_attribute", "make_integrated_address", "get_address_book_entries"] {
            assert!(!ALLOWED.contains(&m), "{m} is refused");
        }
    }

    /// The address book through a real `sm64coin-wallet-rpc` (the sidecar the app drives) beside an ISOLATED node
    /// (`sm64coind --offline --sm64-solo`, fresh data dir, loopback ports: never the real chain). Skips without the binaries
    /// (SM64COIND, SM64COIN_WALLET_RPC, SM64COIN_ROM, or an unpacked release bundle in ~/apps).
    #[test]
    fn address_book_crud_through_the_sidecar() {
        use std::process::{Child, Stdio};
        struct Kill(Child);
        impl Drop for Kill {
            fn drop(&mut self) {
                let _ = self.0.kill(); // only the children this test started
                let _ = self.0.wait();
            }
        }
        let bundle = |name: &str| -> Option<std::path::PathBuf> {
            let home = std::env::var("HOME").ok()?;
            let mut dirs: Vec<_> = std::fs::read_dir(format!("{home}/apps")).ok()?.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("sm64coin-"))).collect();
            dirs.sort();
            dirs.into_iter().rev().map(|d| d.join(name)).find(|p| p.is_file())
        };
        let pick = |var: &str, name: &str| std::env::var_os(var).map(PathBuf::from).or_else(|| bundle(name));
        let (Some(daemon), Some(wrpc), Some(rom)) = (pick("SM64COIND", "sm64coind"), pick("SM64COIN_WALLET_RPC", "sm64coin-wallet-rpc"), pick("SM64COIN_ROM", "baserom.us.z64")) else {
            eprintln!("SKIPPED: set SM64COIND, SM64COIN_WALLET_RPC and SM64COIN_ROM (or unpack a release bundle in ~/apps)");
            return;
        };
        let free = || std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let dir = std::env::temp_dir().join(format!("sm64coin-ab-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("wallets")).unwrap();
        let (rpc_port, p2p, zmq, wport) = (free(), free(), free(), free());
        let _node = Kill(
            Command::new(&daemon)
                .args(["--offline", "--sm64-solo", "--no-igd", "--non-interactive", "--data-dir"])
                .arg(dir.join("node"))
                .arg("--sm64-rom")
                .arg(&rom)
                .args(["--p2p-bind-ip", "127.0.0.1", "--rpc-bind-ip", "127.0.0.1", "--rpc-bind-port", &rpc_port.to_string(), "--p2p-bind-port", &p2p.to_string(), "--zmq-rpc-bind-port", &zmq.to_string(), "--log-file"])
                .arg(dir.join("node.log"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start sm64coind"),
        );
        let (user, pass) = ("test".to_string(), "test-login-not-a-secret".to_string());
        let cfg = dir.join("wrpc.conf");
        std::fs::write(&cfg, format!("rpc-login={user}:{pass}\n")).unwrap();
        let _w = Kill(
            Command::new(&wrpc)
                .arg("--config-file").arg(&cfg).arg("--wallet-dir").arg(dir.join("wallets"))
                .args(["--rpc-bind-ip", "127.0.0.1", "--rpc-bind-port", &wport.to_string(), "--daemon-address", &format!("127.0.0.1:{rpc_port}"), "--trusted-daemon", "--kdf-rounds", "1", "--non-interactive", "--no-dns", "--log-file"])
                .arg(dir.join("wrpc.log"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start wallet-rpc"),
        );
        let t0 = Instant::now();
        let rpc = |method: &str, params: Value| -> Value { digest_post(wport, &user, &pass, &json!({"jsonrpc": "2.0", "id": "0", "method": method, "params": params}).to_string(), Duration::from_secs(60)).unwrap() };
        while digest_post(wport, &user, &pass, r#"{"jsonrpc":"2.0","id":"0","method":"get_version"}"#, Duration::from_secs(3)).is_err() {
            assert!(t0.elapsed() < Duration::from_secs(60), "wallet-rpc did not answer");
            std::thread::sleep(Duration::from_millis(300));
        }
        let made = rpc("create_wallet", json!({"filename": "ab", "password": "", "language": "English"}));
        assert!(made.get("error").is_none(), "{made}");
        let me = rpc("get_address", json!({"account_index": 0}))["result"]["address"].as_str().unwrap().to_string();
        const A: &str = "SM64RtPqsHM63LZn2u9Myx9HtC4kjhu1h98SRtJz9USB7NZAoCMdZSBD7D1L2DWu9uCYZFUSVKytreuzEMJ6VjBU2tbjKqevFH";

        // validation: what the app's add/edit gate asks first
        assert!(address_ok(&rpc("validate_address", json!({"address": me, "any_net_type": false}))), "our own address is valid");
        assert!(!address_ok(&rpc("validate_address", json!({"address": "4AdUndXHHZ6cfufTMvppY6JwXNouMBzSkbLYfpAV5Usx3skxNgYeYTRj5UzqtReoS44qo9mtmXCqY45DJ852K5Jv2684Rge", "any_net_type": false}))), "a Monero address is not ours");
        assert!(!address_ok(&rpc("validate_address", json!({"address": "not-an-address", "any_net_type": false}))));

        // empty at first
        let list = |_: ()| rpc("get_address_book", json!({"entries": []}))["result"]["entries"].as_array().cloned().unwrap_or_default();
        assert!(list(()).is_empty());
        // create
        let added = rpc("add_address_book", json!({"address": A, "description": "Alice"}));
        assert!(added.get("error").is_none(), "{added}");
        let i1 = added["result"]["index"].as_u64().unwrap();
        let added2 = rpc("add_address_book", json!({"address": me, "description": "Me again"}));
        let i2 = added2["result"]["index"].as_u64().unwrap();
        assert_ne!(i1, i2);
        let l = list(());
        assert_eq!(l.len(), 2);
        let alice = l.iter().find(|e| e["index"].as_u64() == Some(i1)).unwrap();
        assert_eq!((alice["address"].as_str().unwrap(), alice["description"].as_str().unwrap()), (A, "Alice"));
        // update: label only, then address too
        let e = rpc("edit_address_book", json!({"index": i1, "set_address": false, "address": "", "set_description": true, "description": "Alice B."}));
        assert!(e.get("error").is_none(), "{e}");
        let l = list(());
        let alice = l.iter().find(|e| e["index"].as_u64() == Some(i1)).unwrap();
        assert_eq!((alice["address"].as_str().unwrap(), alice["description"].as_str().unwrap()), (A, "Alice B."));
        let e = rpc("edit_address_book", json!({"index": i2, "set_address": true, "address": A, "set_description": false, "description": ""}));
        assert!(e.get("error").is_none(), "{e}");
        assert_eq!(list(()).iter().find(|e| e["index"].as_u64() == Some(i2)).unwrap()["address"].as_str().unwrap(), A);
        // it is stored in the (encrypted) wallet file: store, close, reopen
        assert!(rpc("store", json!({}))["error"].is_null());
        assert!(rpc("close_wallet", json!({}))["error"].is_null());
        assert!(rpc("open_wallet", json!({"filename": "ab", "password": ""}))["error"].is_null());
        assert_eq!(list(()).len(), 2, "the address book survives a reopen");
        // delete
        let d = rpc("delete_address_book", json!({"index": i1}));
        assert!(d.get("error").is_none(), "{d}");
        let l = list(());
        assert_eq!(l.len(), 1);
        assert_eq!(l[0]["description"].as_str().unwrap(), "Me again");

        // quick-unlock's primitives against the real wallet file: a PIN seals a random password that replaces the empty one;
        // the wallet then opens only with it (and the PIN gives it back), and the address book is still inside
        let secret = random_password();
        let pinfile = dir.join("ab.pin");
        walletpin::save(&pinfile, &walletpin::wrap(&secret, "2468", "ab", 1000, 5, true).unwrap()).unwrap();
        let c = rpc("change_wallet_password", json!({"old_password": "", "new_password": secret.as_str()}));
        assert!(c.get("error").is_none(), "{c}");
        assert!(rpc("store", json!({}))["error"].is_null());
        assert!(rpc("close_wallet", json!({}))["error"].is_null());
        assert!(rpc("open_wallet", json!({"filename": "ab", "password": ""}))["error"].is_object(), "the empty password no longer opens it");
        assert!(matches!(walletpin::attempt(&pinfile, "1111", "ab").unwrap(), Attempt::Wrong { left: 4 }));
        let Attempt::Ok(got) = walletpin::attempt(&pinfile, "2468", "ab").unwrap() else { panic!("the PIN opens the sealed password") };
        let o = rpc("open_wallet", json!({"filename": "ab", "password": got.as_str()}));
        assert!(o.get("error").is_none(), "{o}");
        assert_eq!(list(()).len(), 1, "the address book is still there");
        // turning it off: a password of the user's choice (here none) replaces the generated one
        let c = rpc("change_wallet_password", json!({"old_password": got.as_str(), "new_password": ""}));
        assert!(c.get("error").is_none(), "{c}");
        assert!(rpc("store", json!({}))["error"].is_null());
        assert!(rpc("close_wallet", json!({}))["error"].is_null());
        assert!(rpc("open_wallet", json!({"filename": "ab", "password": ""}))["error"].is_null());
        let _ = std::fs::remove_dir_all(&dir);
    }

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
