//! `sm64coin`: the one-command launcher of the SM64Coin release bundle.
//!
//! 1. finds the ROM (checks its SHA-1) next to the program, in `~/.sm64coin/` or at a given path,
//! 2. starts the node (`sm64coind`) with the peers of `seeds.txt`, data in `~/.sm64coin/node`,
//! 3. serves the website (`web/`) locally and proxies `/json_rpc` to the node (and
//!    `/leaderboard-api` to a leaderboard server, if one is configured or started),
//! 4. opens the browser and the wallet app, pointed at that node,
//! 5. on Ctrl-C (or when the node dies) stops everything it started.

use clap::Parser;

mod dht;
use sha1::{Digest, Sha1};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const ROM_SHA1: &str = "9bef1128717f958171a4afac3ed78ee2bb4e86ce";
const ROM_NAME: &str = "baserom.us.z64";
const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

/// The release's version: the tag CI builds (`VERSION`, e.g. v0.2.2), else Cargo's.
fn version() -> &'static str {
    match option_env!("VERSION") {
        Some(v) if !v.is_empty() => v.trim_start_matches('v'),
        _ => env!("CARGO_PKG_VERSION"),
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "sm64coin",
    version = version(),
    about = "Runs an SM64Coin node, the website and the wallet, with one command.",
    after_help = "You need your own US Super Mario 64 ROM (baserom.us.z64, SHA-1 9bef1128717f958171a4afac3ed78ee2bb4e86ce). \
It is searched for next to this program, in the current directory and in ~/.sm64coin/."
)]
struct Opts {
    /// Path of the ROM (or a folder holding it); default: look next to the program and in ~/.sm64coin/
    rom: Option<PathBuf>,
    /// Where the node's blockchain, logs and settings live
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Extra peer to join (host:port), repeatable; seeds.txt is always read
    #[arg(long = "peer", value_name = "HOST:PORT")]
    peers: Vec<String>,
    /// Seeds file (one host:port per line); default seeds.txt next to the program and in the data dir
    #[arg(long, value_name = "FILE")]
    seeds: Option<PathBuf>,
    /// First node of a brand-new network (counts as synchronized without peers)
    #[arg(long)]
    solo: bool,
    /// Don't look for nodes in the BitTorrent Mainline DHT (seeds.txt and --peer only)
    #[arg(long)]
    no_dht: bool,
    /// Announce this node in the DHT so others find it (it must accept connections on the p2p port;
    /// your IP is then listed for anyone who looks the network up)
    #[arg(long)]
    dht_announce: bool,
    /// Print the nodes the DHT knows and exit
    #[arg(long)]
    dht_lookup: bool,
    #[arg(long, default_value_t = 16480)]
    p2p_port: u16,
    #[arg(long, default_value_t = 16481)]
    rpc_port: u16,
    #[arg(long, default_value_t = 16482)]
    zmq_port: u16,
    /// Port of the local website
    #[arg(long, default_value_t = 5181)]
    web_port: u16,
    /// Serve the website on all interfaces, not just this computer
    #[arg(long)]
    public: bool,
    /// Folder of the built website; default `web/` next to the program
    #[arg(long, value_name = "DIR")]
    web_dir: Option<PathBuf>,
    /// Do not open the website in the browser
    #[arg(long)]
    no_browser: bool,
    /// Do not start the wallet app
    #[arg(long)]
    no_wallet: bool,
    /// Use this remote leaderboard server (e.g. http://host:16490) instead of running one here
    #[arg(long, value_name = "URL")]
    leaderboard_url: Option<String>,
    /// Port of the leaderboard server this program runs (it shares the top 50 with other leaderboard servers)
    #[arg(long, value_name = "PORT", default_value_t = 16490)]
    leaderboard_port: u16,
    /// Do not run a leaderboard server
    #[arg(long)]
    no_leaderboard: bool,
    /// Another leaderboard server to share with (URL or host:port), repeatable; leaderboard-peers.txt is always read
    #[arg(long = "peer-leaderboard", value_name = "URL")]
    peer_leaderboards: Vec<String>,
    /// Also run the automated miner (sm64coin-automine), mining to this address
    #[arg(long, value_name = "SM64...")]
    mine: Option<String>,
    /// Extra argument for sm64coind, repeatable
    #[arg(long = "daemon-arg", value_name = "ARG", allow_hyphen_values = true)]
    daemon_args: Vec<String>,
}

/// The website's origin (GitHub Pages), allowed to call the local node's RPC.
const SITE_ORIGIN: &str = "https://gumbo64.github.io";

fn main() {
    let code = match run(Opts::parse()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("\nsm64coin: {e}");
            2
        }
    };
    pause_if_console_would_close();
    std::process::exit(code);
}

/// A double-clicked Windows program closes its console at exit: keep the message readable.
/// Only when this process is alone in its console (started from Explorer, not from cmd/PowerShell
/// and not by a script that closes stdin).
fn pause_if_console_would_close() {
    #[cfg(windows)]
    {
        use std::io::IsTerminal;
        unsafe extern "system" {
            fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
        }
        let mut ids = [0u32; 4];
        let n = unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) };
        if n <= 1 && std::io::stdin().is_terminal() {
            eprintln!("\nPress Enter to close.");
            let mut s = String::new();
            let _ = std::io::stdin().read_line(&mut s);
        }
    }
}

/// Windows: put every child into a job object that kills them when this program ends for any
/// reason (console window closed, killed), so no node or wallet is left running unseen.
#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::*;

    struct Job(HANDLE);
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}
    static JOB: OnceLock<Job> = OnceLock::new();

    pub fn adopt(child: &std::process::Child) {
        let job = JOB.get_or_init(|| unsafe {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(h, JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32);
            Job(h)
        });
        unsafe {
            AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE);
        }
    }
}

/// Paths from canonicalize() carry a `\\?\` prefix on Windows that some programs cannot read.
fn plain(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(r) if !r.starts_with("UNC") => PathBuf::from(r),
        _ => p,
    }
}

fn home() -> PathBuf {
    // Windows: the user's profile (a HOME variable only exists inside MSYS/Git shells).
    let (a, b) = if cfg!(windows) { ("USERPROFILE", "HOME") } else { ("HOME", "USERPROFILE") };
    std::env::var_os(a).or_else(|| std::env::var_os(b)).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn sha1_file(p: &Path) -> std::io::Result<String> {
    let mut f = File::open(p)?;
    let mut h = Sha1::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Look for the ROM; Ok(path) when one with the right SHA-1 is found.
fn find_rom(arg: Option<&Path>, exe_dir: &Path, data_dir: &Path) -> Result<PathBuf, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    let add = |p: PathBuf, files: &mut Vec<PathBuf>, dirs: &mut Vec<PathBuf>| {
        if p.is_dir() { dirs.push(p) } else { files.push(p) }
    };
    if let Some(a) = arg {
        add(a.to_path_buf(), &mut files, &mut dirs);
        if !a.exists() {
            return Err(format!("the ROM path {} does not exist", a.display()));
        }
    }
    if let Some(e) = std::env::var_os("SM64COIN_ROM") {
        add(PathBuf::from(e), &mut files, &mut dirs);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    for d in [exe_dir.to_path_buf(), cwd, home().join(".sm64coin"), data_dir.to_path_buf()] {
        dirs.push(d);
    }
    for d in &dirs {
        files.push(d.join(ROM_NAME));
    }
    // Any other .z64 in those folders (a renamed copy, "Super Mario 64 (U) [!].z64").
    for d in &dirs {
        if let Ok(rd) = fs::read_dir(d) {
            let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("z64"))).collect();
            v.sort();
            files.extend(v);
        }
    }
    let mut seen = BTreeSet::new();
    let mut wrong: Vec<(PathBuf, String)> = Vec::new();
    for f in files {
        let key = plain(fs::canonicalize(&f).unwrap_or_else(|_| f.clone()));
        if !f.is_file() || !seen.insert(key.clone()) {
            continue;
        }
        match sha1_file(&f) {
            Ok(h) if h == ROM_SHA1 => return Ok(key),
            Ok(h) => wrong.push((f, h)),
            Err(_) => {}
        }
    }
    let mut msg = String::new();
    msg.push_str("Could not find your Super Mario 64 ROM.\n\n");
    msg.push_str(&format!("SM64Coin needs the US version of Super Mario 64 as a .z64 file (SHA-1 {ROM_SHA1}).\n"));
    msg.push_str("It is not included: you have to supply your own copy.\n\n");
    for (p, h) in &wrong {
        msg.push_str(&format!("  Found {} but it is not the US ROM (its SHA-1 is {h}).\n", p.display()));
    }
    msg.push_str(&format!(
        "Do one of these and run sm64coin again:\n  - copy it next to this program, named {ROM_NAME}  ({})\n  - or copy it to {}\n  - or give its path:  sm64coin /path/to/rom.z64\n",
        exe_dir.display(),
        home().join(".sm64coin").join(ROM_NAME).display()
    ));
    Err(msg)
}

/// `host:port` (port 16480 if missing) as `ip:port`, or None (with a note) if it doesn't resolve.
fn resolve_peer(p: &str) -> Option<String> {
    use std::net::ToSocketAddrs;
    let with_port = if p.rsplit_once(':').is_some_and(|(_, port)| port.parse::<u16>().is_ok()) { p.to_string() } else { format!("{p}:16480") };
    match with_port.to_socket_addrs() {
        Ok(mut a) => match a.find(|x| x.is_ipv4()) {
            Some(x) => Some(x.to_string()),
            None => {
                println!("seed {p}: no IPv4 address, skipped");
                None
            }
        },
        Err(e) => {
            println!("seed {p} doesn't resolve ({e}), skipped");
            None
        }
    }
}

fn read_seeds(p: &Path) -> Vec<String> {
    fs::read_to_string(p)
        .map(|s| s.lines().map(|l| l.split('#').next().unwrap_or("").split_whitespace().collect::<String>()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

struct Proc {
    name: &'static str,
    child: Child,
    log: PathBuf,
    /// Windows has no SIGTERM: this HTTP address asks the program to exit by itself first.
    stop_url: Option<String>,
}

impl Proc {
    fn spawn(name: &'static str, mut cmd: Command, log: PathBuf) -> Result<Proc, String> {
        let out = File::create(&log).map_err(|e| format!("cannot write {}: {e}", log.display()))?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        cmd.stdin(Stdio::null()).stdout(out).stderr(err);
        let child = cmd.spawn().map_err(|e| format!("cannot start {name}: {e}"))?;
        #[cfg(windows)]
        job::adopt(&child);
        Ok(Proc { name, child, log, stop_url: None })
    }
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
    fn tail(&self) -> String {
        let s = fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<&str> = s.lines().collect();
        lines[lines.len().saturating_sub(12)..].join("\n")
    }
    /// Ask politely (SIGTERM), wait up to `secs`, then kill.
    fn stop(&mut self, secs: u64) {
        if !self.alive() {
            return;
        }
        println!("stopping {} ...", self.name);
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        if let Some(u) = &self.stop_url {
            let _ = ureq::post(u).timeout(Duration::from_secs(5)).send_string("{}");
        } else {
            let _ = self.child.kill();
        }
        let t = Instant::now();
        while self.alive() && t.elapsed() < Duration::from_secs(secs) {
            thread::sleep(Duration::from_millis(200));
        }
        if self.alive() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn rpc_get_info(rpc_port: u16) -> Option<serde_json::Value> {
    let r = ureq::post(&format!("http://127.0.0.1:{rpc_port}/json_rpc"))
        .timeout(Duration::from_secs(3))
        .send_string(r#"{"jsonrpc":"2.0","id":0,"method":"get_info"}"#)
        .ok()?;
    let v: serde_json::Value = serde_json::from_reader(r.into_reader()).ok()?;
    v.get("result").cloned()
}

/// The peers the node has finished a handshake with ("a.b.c.d:16480", " in" if they connected to
/// us); None if the RPC doesn't answer (a restricted RPC lists none).
fn peers(rpc_port: u16) -> Option<Vec<String>> {
    let r = ureq::post(&format!("http://127.0.0.1:{rpc_port}/json_rpc"))
        .timeout(Duration::from_secs(3))
        .send_string(r#"{"jsonrpc":"2.0","id":0,"method":"get_connections"}"#)
        .ok()?;
    let v: serde_json::Value = serde_json::from_reader(r.into_reader()).ok()?;
    let list = v["result"]["connections"].as_array().cloned().unwrap_or_default();
    Some(
        list.iter()
            .filter(|c| c["state"].as_str().is_some_and(|s| s != "before_handshake"))
            .filter_map(|c| c["address"].as_str().map(|a| format!("{a}{}", if c["incoming"].as_bool() == Some(true) { " in" } else { "" })))
            .collect(),
    )
}

fn port_free(port: u16, host: &str) -> bool {
    std::net::TcpListener::bind((host, port)).is_ok()
}

/// A free port: `preferred` if free, else one of the next 20, else any port the
/// system hands out. `taken` are ports this launcher already chose.
fn pick_port(preferred: u16, host: &str, taken: &[u16]) -> u16 {
    let ok = |p: u16| p != 0 && !taken.contains(&p) && port_free(p, host);
    if let Some(p) = (0..=20u16).map(|i| preferred.saturating_add(i)).find(|&p| ok(p)) {
        return p;
    }
    for _ in 0..20 {
        if let Ok(l) = std::net::TcpListener::bind((host, 0)) {
            if let Ok(a) = l.local_addr() {
                if !taken.contains(&a.port()) {
                    return a.port();
                }
            }
        }
    }
    preferred
}

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "xor" | "zip" | "bin" => "application/octet-stream",
        _ => "application/octet-stream",
    }
}

struct WebCfg {
    dir: PathBuf,
    rpc: String,
    leaderboard: Option<String>,
    /// Served on all interfaces (--public): other computers use it by IP address.
    public: bool,
    /// The port the site is actually served on (set once bound).
    port: u16,
}

/// Largest request body the proxies take (submit_block bodies are < ~600 KB of hex).
const MAX_BODY: usize = 1 << 20;
/// Largest upstream answer the proxies relay.
const MAX_RESPONSE: u64 = 64 << 20;
/// The daemon calls the website needs. The node's RPC is unrestricted (stop_daemon, set_bans,
/// prune_blockchain ...), so the proxy only passes these.
const RPC_ALLOWED: &[&str] = &[
    "get_info", "get_block_count", "get_block", "get_block_header_by_hash", "get_block_header_by_height",
    "get_block_headers_range", "get_block_template", "submit_block",
    // proof-of-play (the miner's parent state and cycle) and the node's leaderboard
    "get_sm64_cycle", "get_sm64_state", "sm64_board", "sm64_board_join",
    // read-only calls a wallet makes (restricted-RPC safe)
    "get_version", "get_fee_estimate", "get_last_block_header", "get_output_distribution", "hard_fork_info",
];

fn host_is_ip(h: &str) -> bool {
    h.starts_with('[') || h.parse::<std::net::IpAddr>().is_ok()
}

/// Splits `host[:port]` (host may be `[::1]`).
fn split_host(h: &str) -> (&str, Option<&str>) {
    if let Some(end) = h.find(']') {
        return (&h[..=end], h[end + 1..].strip_prefix(':'));
    }
    match h.rsplit_once(':') {
        Some((a, b)) => (a, Some(b)),
        None => (h, None),
    }
}

/// Browser-driven attacks on the local node: a foreign page can POST to 127.0.0.1 (its Origin
/// says so), and DNS rebinding makes a foreign name resolve to us (its Host says so).
/// None if the request is fine, else the reason.
fn forbidden(origin: Option<&str>, host: Option<&str>, port: u16, public: bool) -> Option<&'static str> {
    let host = host.unwrap_or("");
    let (name, hp) = split_host(host);
    let local = matches!(name.to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "[::1]");
    if hp.is_some_and(|p| p != port.to_string()) || (hp.is_none() && port != 80) {
        return Some("wrong Host");
    }
    // With --public others come by IP address (never by a name: that is what rebinding needs).
    if !local && !(public && host_is_ip(name)) {
        return Some("wrong Host");
    }
    if let Some(o) = origin {
        let o = o.trim_end_matches('/');
        let own = [format!("http://localhost:{port}"), format!("http://127.0.0.1:{port}"), format!("http://[::1]:{port}")];
        let same = public && o.eq_ignore_ascii_case(&format!("http://{host}"));
        if !own.iter().any(|a| a.eq_ignore_ascii_case(o)) && !same {
            return Some("foreign Origin");
        }
    }
    None
}

fn header(k: &str, v: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn respond_text(req: tiny_http::Request, code: u16, body: &str) {
    let _ = req.respond(tiny_http::Response::from_string(body).with_status_code(code).with_header(header("Content-Type", "text/plain; charset=utf-8")));
}

fn proxy(mut req: tiny_http::Request, base: &str, rest: &str) {
    if req.body_length().is_some_and(|n| n > MAX_BODY) {
        return respond_text(req, 413, "request too large");
    }
    let mut body = Vec::new();
    let _ = req.as_reader().take(MAX_BODY as u64 + 1).read_to_end(&mut body);
    if body.len() > MAX_BODY {
        return respond_text(req, 413, "request too large");
    }
    if rest == "/json_rpc" {
        // Only the calls the website makes (one object, no batches).
        let m = serde_json::from_slice::<serde_json::Value>(&body).ok().and_then(|v| v.get("method").and_then(|m| m.as_str().map(String::from)));
        if !m.as_deref().is_some_and(|m| RPC_ALLOWED.contains(&m)) {
            return respond_text(req, 403, "this RPC method is not available through the website");
        }
    }
    let url = format!("{base}{rest}");
    let method = req.method().as_str().to_string();
    let mut r = ureq::request(&method, &url).timeout(Duration::from_secs(60));
    for h in req.headers() {
        let k = h.field.as_str().as_str().to_ascii_lowercase();
        if k == "content-type" || k == "accept" {
            r = r.set(h.field.as_str().as_str(), h.value.as_str());
        }
    }
    let res = if body.is_empty() && method == "GET" { r.call() } else { r.send_bytes(&body) };
    let resp = match res {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return respond_text(req, 502, &format!("upstream unavailable: {e}")),
    };
    let code = resp.status();
    let ct = resp.header("content-type").unwrap_or("application/json").to_string();
    let mut data = Vec::new();
    let _ = resp.into_reader().take(MAX_RESPONSE).read_to_end(&mut data);
    let _ = req.respond(tiny_http::Response::from_data(data).with_status_code(code).with_header(header("Content-Type", &ct)));
}

/// Proxied requests in flight. Each runs on its own thread: a node busy replaying (or starved by
/// the miner) answers slowly, and on the shared worker threads a few slow calls kept the page's
/// own files from loading.
static PROXIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const MAX_PROXIES: usize = 64;

fn proxy_async(req: tiny_http::Request, base: String, rest: String) {
    if PROXIES.fetch_add(1, Ordering::SeqCst) >= MAX_PROXIES {
        PROXIES.fetch_sub(1, Ordering::SeqCst);
        return respond_text(req, 503, "the node is busy: too many requests waiting for it");
    }
    thread::spawn(move || {
        proxy(req, &base, &rest);
        PROXIES.fetch_sub(1, Ordering::SeqCst);
    });
}

fn handle(req: tiny_http::Request, cfg: &WebCfg) {
    let hv = |name: &'static str| req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().to_string());
    if let Some(why) = forbidden(hv("Origin").as_deref(), hv("Host").as_deref(), cfg.port, cfg.public) {
        return respond_text(req, 403, why);
    }
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').map(|(p, q)| (p.to_string(), format!("?{q}"))).unwrap_or((url.clone(), String::new()));
    if path == "/json_rpc" {
        return proxy_async(req, cfg.rpc.clone(), "/json_rpc".into());
    }
    if let Some(rest) = path.strip_prefix("/leaderboard-api") {
        return match &cfg.leaderboard {
            Some(base) => proxy_async(req, base.clone(), format!("{}{}", if rest.is_empty() { "/" } else { rest }, query)),
            None => respond_text(req, 503, "no leaderboard configured (start sm64coin without --no-leaderboard, or with --leaderboard-url)"),
        };
    }
    let rel = path.trim_start_matches('/');
    let ok_path = !rel.split(['/', '\\']).any(|c| c == ".." || c.contains(':'));
    let mut file = cfg.dir.join(if rel.is_empty() { "index.html" } else { rel });
    if !ok_path {
        return respond_text(req, 400, "bad path");
    }
    if file.is_dir() {
        file = file.join("index.html");
    }
    // Single-page app: unknown paths without an extension are routes of the site.
    if !file.is_file() && Path::new(rel).extension().is_none() {
        file = cfg.dir.join("index.html");
    }
    match File::open(&file) {
        Ok(f) if file.is_file() => {
            let _ = req.respond(tiny_http::Response::from_file(f).with_header(header("Content-Type", content_type(&file))));
        }
        _ => respond_text(req, 404, "not found"),
    }
}

/// Serves the website on `port`, or the next free port if it is taken (by an
/// earlier run, another program, an editor's port forwarding...). Returns the port.
fn serve_web(host: &str, port: u16, cfg: WebCfg) -> Result<u16, String> {
    let mut last = String::new();
    let mut server = None;
    for p in (0..=20u16).map(|i| port.saturating_add(i)).chain(std::iter::once(0)) {
        match tiny_http::Server::http((host, p)) {
            Ok(s) => {
                let actual = s.server_addr().to_ip().map(|a| a.port()).unwrap_or(p);
                server = Some((s, actual));
                break;
            }
            Err(e) => last = e.to_string(),
        }
    }
    let (server, port) = server.ok_or_else(|| format!("cannot serve the website on {host} (port {port} and the next ones): {last}"))?;
    let server = Arc::new(server);
    let mut cfg = cfg;
    cfg.port = port;
    let cfg = Arc::new(cfg);
    for _ in 0..8 {
        let (s, c) = (server.clone(), cfg.clone());
        thread::spawn(move || {
            while let Ok(req) = s.recv() {
                handle(req, &c);
            }
        });
    }
    Ok(port)
}

/// The wallet app has no command-line option for its node: it reads its settings file.
/// First run only (never overwrite the user's choices): use the node this launcher runs.
/// (Linux path; elsewhere the wizard asks: choose a remote node at 127.0.0.1:<rpc port>.)
fn seed_wallet_settings(rpc_port: u16) {
    if !cfg!(target_os = "linux") {
        println!("In the wallet's first-run wizard choose \"remote node\" 127.0.0.1:{rpc_port}.");
        return;
    }
    let dir = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("sm64coin");
    let f = dir.join("sm64coin-wallet.conf");
    let nodes = format!(r#"{{\"selected\":0,\"nodes\":[{{\"address\":\"127.0.0.1:{rpc_port}\",\"username\":\"\",\"password\":\"\",\"trusted\":true}}]}}"#);
    let line = format!("remoteNodesSerialized=\"{nodes}\"");
    if let Ok(text) = fs::read_to_string(&f) {
        // Ours (written by this launcher, marked) and the node moved to another port: follow it.
        // Never touch settings the user made themselves.
        if text.contains("sm64coinLauncherNode=true") {
            let updated: Vec<String> = text.lines().map(|l| if l.starts_with("remoteNodesSerialized=") { line.clone() } else { l.to_string() }).collect();
            let _ = fs::write(&f, updated.join("\n") + "\n");
        } else {
            println!("(wallet settings exist: if it does not find the node, use remote node 127.0.0.1:{rpc_port} in its settings)");
        }
        return;
    }
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(&f, format!("[General]\ncheckForUpdates=false\nuseRemoteNode=true\nsm64coinLauncherNode=true\n{line}\n"));
}

/// The miner uses every core but one; at normal priority it starved the node and the website.
/// Lowered right after spawning (its search children inherit it).
fn lower_priority(child: &Child) {
    #[cfg(unix)]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, child.id() as libc::id_t, 10);
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        unsafe extern "system" {
            fn SetPriorityClass(h: *mut std::ffi::c_void, class: u32) -> i32;
        }
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x4000;
        unsafe {
            SetPriorityClass(child.as_raw_handle() as *mut _, BELOW_NORMAL_PRIORITY_CLASS);
        }
    }
}

fn open_browser(url: &str) {
    let r = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
    } else if cfg!(windows) {
        Command::new("cmd").args(["/C", "start", "", url]).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
    } else {
        Command::new("xdg-open").arg(url).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
    };
    if r.is_err() {
        println!("(could not open a browser by itself: open the address above yourself)");
    }
}

fn run(o: Opts) -> Result<(), String> {
    if o.dht_lookup {
        if std::env::var_os("SM64COIN_DHT_ANNOUNCE_TEST").is_some() {
            dht::announce_forever(o.p2p_port);
            std::thread::sleep(Duration::from_secs(90));
            return Ok(());
        }
        println!("DHT key {} ({}):", std::env::var("SM64COIN_DHT_NETWORK").unwrap_or_else(|_| dht::NETWORK.into()), dht::info_hash());
        for a in dht::lookup(Duration::from_secs(30), 200) {
            println!("{a}");
        }
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_dir = plain(fs::canonicalize(&exe).unwrap_or(exe)).parent().map(Path::to_path_buf).unwrap_or_default();
    let data_dir = o.data_dir.clone().unwrap_or_else(|| home().join(".sm64coin"));
    let tool = |n: &str| -> PathBuf { exe_dir.join(format!("{n}{EXE}")) };

    println!("SM64Coin {}", version());
    // CI only: SM64COIN_TEST_NO_ROM=1 skips the ROM search so the launcher's own plumbing (website,
    // /json_rpc proxy, child processes, shutdown) can be tested against a stand-in node.
    let rom = if std::env::var_os("SM64COIN_TEST_NO_ROM").is_some() { PathBuf::from("no-rom-test") } else { find_rom(o.rom.as_deref(), &exe_dir, &data_dir)? };
    println!("ROM: {} (SHA-1 ok)", rom.display());

    let daemon_bin = tool("sm64coind");
    let have_daemon = daemon_bin.is_file();
    let web_dir = o.web_dir.clone().unwrap_or_else(|| exe_dir.join("web"));
    if !web_dir.join("index.html").is_file() {
        return Err(format!("the website is missing: {} has no index.html", web_dir.display()));
    }
    let node_dir = data_dir.join("node");
    let log_dir = data_dir.join("logs");
    fs::create_dir_all(&node_dir).and_then(|_| fs::create_dir_all(&log_dir)).map_err(|e| format!("cannot create {}: {e}", data_dir.display()))?;

    // Ctrl-C / SIGTERM / console close: stop and clean up.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let s = stop.clone();
        ctrlc::set_handler(move || s.store(true, Ordering::SeqCst)).map_err(|e| e.to_string())?;
    }

    let mut procs: Vec<Proc> = Vec::new();
    let mut started_node = false;
    // Ports actually used: the defaults, or free ones nearby when those are taken.
    let mut o = o;
    let reuse_node = rpc_get_info(o.rpc_port).is_some();
    if !reuse_node {
        let mut taken: Vec<u16> = Vec::new();
        for (name, port) in [("P2P", &mut o.p2p_port), ("RPC", &mut o.rpc_port), ("ZMQ", &mut o.zmq_port)] {
            let p = pick_port(*port, "127.0.0.1", &taken);
            if p != *port {
                println!("{name} port {} is in use by another program: using {p}.", *port);
                *port = p;
            }
            taken.push(p);
        }
    }
    let rpc_url = format!("http://127.0.0.1:{}", o.rpc_port);

    let result = (|| -> Result<(), String> {
        // --- node ---
        if reuse_node {
            println!("A node is already running on RPC port {}: using it (it is left running when you exit).", o.rpc_port);
        } else {
            if !have_daemon {
                return Err(format!("{} is missing", daemon_bin.display()));
            }
            let mut peers: Vec<String> = Vec::new();
            let seed_files = match &o.seeds {
                Some(f) => vec![f.clone()],
                None => vec![exe_dir.join("seeds.txt"), data_dir.join("seeds.txt")],
            };
            for f in &seed_files {
                peers.extend(read_seeds(f));
            }
            peers.extend(o.peers.iter().cloned());
            // Hostnames resolved here; one that doesn't resolve is skipped (the daemon refuses
            // to start on an --add-peer it can't resolve).
            let named = peers.clone();
            let mut peers: Vec<String> = peers.iter().filter_map(|p| resolve_peer(p)).collect();
            peers.sort();
            peers.dedup();
            if !named.is_empty() {
                let mut n = named.clone();
                n.dedup();
                println!("Joining through: {}", n.join(", "));
            }
            // Nodes announced in the Mainline DHT (dht.rs): plain --add-peer, the node's own peer
            // exchange does the rest.
            let mut dht_peers: Vec<String> = Vec::new();
            if !o.no_dht {
                print!("Looking for nodes in the DHT ... ");
                let _ = std::io::stdout().flush();
                dht_peers = dht::lookup(Duration::from_secs(20), 24).iter().map(|a| a.to_string()).filter(|a| !peers.contains(a)).collect();
                println!("{} found.", dht_peers.len());
            }
            let mut cmd = Command::new(&daemon_bin);
            cmd.arg("--data-dir").arg(&node_dir).arg("--sm64-rom").arg(&rom);
            cmd.args(["--p2p-bind-port", &o.p2p_port.to_string(), "--rpc-bind-port", &o.rpc_port.to_string(), "--zmq-rpc-bind-port", &o.zmq_port.to_string()]);
            // The website on GitHub Pages calls this node from the visitor's browser (RPC stays on 127.0.0.1).
            if !o.daemon_args.iter().any(|x| x.starts_with("--rpc-access-control-origins")) {
                cmd.args(["--rpc-access-control-origins", SITE_ORIGIN]);
            }
            // Monero allows one inbound connection per IP; with few seeds that locks out a second node
            // behind the same home router or carrier NAT. A flag, not a code change.
            if !o.daemon_args.iter().any(|x| x.starts_with("--max-connections-per-ip")) {
                cmd.arg("--max-connections-per-ip=16");
            }
            cmd.arg("--non-interactive").arg("--log-file").arg(log_dir.join("node-daemon.log"));
            if o.solo {
                cmd.arg("--sm64-solo");
            }
            for p in &peers {
                // In the peer list, and kept connected (a node alone on a new network has nothing else to find).
                cmd.args(["--add-peer", p, "--add-priority-node", p]);
            }
            for p in &dht_peers {
                cmd.args(["--add-peer", p]);
            }
            cmd.args(&o.daemon_args);
            if o.dht_announce {
                dht::announce_forever(o.p2p_port);
            }
            if peers.is_empty() && dht_peers.is_empty() && !o.solo {
                println!("Note: no seed node resolved and the DHT found none. Is this computer online? Pass --peer HOST:PORT to join through a node you know.");
            }
            println!("Starting the node (data in {}) ...", node_dir.display());
            // The rest starts now, not when the node answers: the site and the wallet wait for it.
            let mut node = Proc::spawn("node", cmd, log_dir.join("node.log"))?;
            node.stop_url = Some(format!("{rpc_url}/stop_daemon"));
            procs.push(node);
            started_node = true;
        }

        // --- leaderboard ---
        // Runs by default: it shares the top 50 with other leaderboard servers (pulling works behind NAT;
        // with --public others can also pull from this one). The website proxies to it.
        let mut lb_url = o.leaderboard_url.clone().map(|u| u.trim_end_matches('/').to_string());
        if lb_url.is_none() && !o.no_leaderboard {
            let bin = tool("sm64coin-leaderboard");
            if !bin.is_file() {
                println!("(no leaderboard server in this bundle: {} is missing)", bin.display());
            } else {
                let bind_host = if o.public { "0.0.0.0" } else { "127.0.0.1" };
                let port = pick_port(o.leaderboard_port, bind_host, &[o.web_port]);
                if port != o.leaderboard_port {
                    println!("Leaderboard port {} is in use by another program: using {port}. (Set the wallet's leaderboard URL to http://127.0.0.1:{port}.)", o.leaderboard_port);
                }
                let lb_data = data_dir.join("leaderboard");
                fs::create_dir_all(&lb_data).map_err(|e| e.to_string())?;
                let mut cmd = Command::new(bin);
                cmd.args(["--daemon", &rpc_url, "--bind", &format!("{bind_host}:{port}"), "--poll-secs", "10"]).arg("--data-dir").arg(&lb_data);
                for u in &o.peer_leaderboards {
                    cmd.args(["--peer-leaderboard", u]);
                }
                for f in [exe_dir.join("leaderboard-peers.txt"), data_dir.join("leaderboard-peers.txt")] {
                    if f.is_file() {
                        cmd.arg("--peers-file").arg(f);
                    }
                }
                cmd.args(["--peer-port", &o.leaderboard_port.to_string()]);
                procs.push(Proc::spawn("leaderboard", cmd, log_dir.join("leaderboard.log"))?);
                lb_url = Some(format!("http://127.0.0.1:{port}"));
                println!("Leaderboard server on port {port}{}.", if o.public { " (public: other servers can share with it)" } else { "" });
            }
        }

        // --- website ---
        let host = if o.public { "0.0.0.0" } else { "127.0.0.1" };
        let web_port = serve_web(host, o.web_port, WebCfg { dir: web_dir, rpc: rpc_url.clone(), leaderboard: lb_url, public: o.public, port: 0 })?;
        if web_port != o.web_port {
            println!("Port {} is in use by another program: the website is on {web_port} instead.", o.web_port);
        }
        let site = format!("http://localhost:{web_port}/");
        println!("Website: {site}");
        if !o.no_browser {
            open_browser(&site);
        }

        // --- wallet app ---
        // macOS: the app bundle (Qt frameworks inside) from the same folder.
        let gui = if cfg!(target_os = "macos") {
            exe_dir.join("SM64Coin-Wallet.app").join("Contents").join("MacOS").join("monero-wallet-gui")
        } else {
            tool("sm64coin-wallet-gui")
        };
        if !o.no_wallet {
            if gui.is_file() {
                let mut cmd = Command::new(&gui);
                // Its Mining page and the node it may start find the same ROM and data folder.
                cmd.env("SM64COIN_ROM", &rom).env("SM64COIN_DATA_DIR", &data_dir);
                // "Play to mine" opens this site (it uses this node), not the GitHub Pages one.
                cmd.env("SM64COIN_WEB_URL", format!("http://localhost:{web_port}"));
                seed_wallet_settings(o.rpc_port);
                match Proc::spawn("wallet", cmd, log_dir.join("wallet-gui.log")) {
                    Ok(p) => {
                        println!("Wallet app started (connect it to the node 127.0.0.1:{}).", o.rpc_port);
                        procs.push(p);
                    }
                    Err(e) => println!("Could not start the wallet app: {e}"),
                }
            } else {
                println!("(no wallet app in this bundle; use sm64coin-wallet-cli)");
            }
        }

        // --- wait for the node ---
        // The website, browser and wallet are up already (they show "connecting" until the node
        // answers: loading a long chain can take a while).
        if started_node {
            let t = Instant::now();
            loop {
                if stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                if !procs[0].alive() {
                    return Err(format!("the node stopped right after starting. Its log ({}):\n{}", procs[0].log.display(), procs[0].tail()));
                }
                if rpc_get_info(o.rpc_port).is_some() {
                    break;
                }
                if t.elapsed() > Duration::from_secs(180) {
                    return Err("the node did not start answering within 3 minutes".into());
                }
                thread::sleep(Duration::from_millis(500));
            }
            println!("Node is up (RPC {rpc_url}).");
        }

        // --- automated miner ---
        if let Some(addr) = &o.mine {
            let bin = tool("sm64coin-automine");
            if !bin.is_file() {
                return Err(format!("--mine: {} is not in this bundle", bin.display()));
            }
            let mut cmd = Command::new(bin);
            cmd.args(["--address", addr, "--daemon", &rpc_url]).arg("--rom").arg(&rom);
            // The campaign's scratch (step files, its memory across blocks, its roadmap) lasts
            // between runs; threads: all but one core.
            cmd.arg("--rrt-scratch").arg(data_dir.join("miner"));
            let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).saturating_sub(1).max(1);
            cmd.arg("--rh-threads").arg(threads.to_string());
            let route = exe_dir.join("routes").join("block1-human.m64");
            if route.is_file() {
                cmd.arg("--route").arg(route);
            }
            let miner = Proc::spawn("miner", cmd, log_dir.join("automine.log"))?;
            lower_priority(&miner.child);
            procs.push(miner);
            println!("Automated miner started, mining to {addr}.");
        }

        println!("\nLogs are in {}. Press Ctrl-C to stop.\n", log_dir.display());

        // --- watch ---
        let mut last_print = Instant::now() - Duration::from_secs(60);
        let mut last_h = (0u64, 0u64);
        let mut synced_said = false;
        let mut last_peers: Option<u64> = None;
        let mut alone_since = Instant::now();
        let mut alone_said = false;
        let mut dead_said: BTreeSet<&'static str> = BTreeSet::new();
        loop {
            if stop.load(Ordering::SeqCst) {
                println!("\nShutting down ...");
                return Ok(());
            }
            if started_node && !procs[0].alive() {
                return Err(format!("the node stopped. Its log ({}):\n{}", procs[0].log.display(), procs[0].tail()));
            }
            // The wallet app, leaderboard or miner quit (a wallet without a display, a crash): say so once.
            for p in procs.iter_mut().skip(if started_node { 1 } else { 0 }) {
                if !p.alive() && dead_said.insert(p.name) {
                    println!("The {} stopped. The end of its log ({}):\n{}", p.name, p.log.display(), p.tail());
                }
            }
            if let Some(i) = rpc_get_info(o.rpc_port) {
                let h = i["height"].as_u64().unwrap_or(0);
                let target = i["target_height"].as_u64().unwrap_or(0).max(h);
                let synced = i["synchronized"].as_bool().unwrap_or(false);
                if synced && !synced_said {
                    println!("Node is synchronized at block {h}.");
                    synced_said = true;
                } else if synced && h > last_h.0 {
                    println!("Block {h}.");
                } else if !synced && ((h, target) != last_h && last_print.elapsed() > Duration::from_secs(10)) {
                    println!("Syncing: block {h} of {}", if target > h { target.to_string() } else { "?".into() });
                    last_print = Instant::now();
                    synced_said = false;
                }
                last_h = (h, target);
                // Who the node is talking to: a new user's first question when it doesn't sync.
                let list = peers(o.rpc_port).unwrap_or_default();
                let n = list.len() as u64;
                if last_peers != Some(n) {
                    if n > 0 {
                        println!("Connected to {n} peer{}: {}.", if n == 1 { "" } else { "s" }, list.join(", "));
                        alone_said = false;
                    } else if last_peers.is_some() {
                        println!("No peers connected (the node keeps trying).");
                    }
                    if n == 0 {
                        alone_since = Instant::now();
                    }
                    last_peers = Some(n);
                }
                if n == 0 && !o.solo && !alone_said && alone_since.elapsed() > Duration::from_secs(60) {
                    println!("Still no peers after a minute: the seed nodes can't be reached from here. Check the internet connection and that a firewall lets sm64coind connect out (TCP 16480), or pass --peer HOST:PORT. Details: {}", log_dir.join("node-daemon.log").display());
                    alone_said = true;
                }
            }
            for _ in 0..4 {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                thread::sleep(Duration::from_millis(500));
            }
        }
    })();

    // Reverse order: miner, wallet, leaderboard, node last.
    for p in procs.iter_mut().rev() {
        p.stop(if p.name == "node" { 120 } else { 15 });
    }
    let _ = std::io::stdout().flush();
    result
}

#[cfg(test)]
mod proxy_guard_tests {
    use super::forbidden;

    #[test]
    fn local_only() {
        assert_eq!(forbidden(None, Some("localhost:5181"), 5181, false), None);
        assert_eq!(forbidden(Some("http://127.0.0.1:5181"), Some("127.0.0.1:5181"), 5181, false), None);
        assert_eq!(forbidden(None, Some("[::1]:5181"), 5181, false), None);
        assert!(forbidden(Some("https://evil.com"), Some("localhost:5181"), 5181, false).is_some());
        assert!(forbidden(Some("https://gumbo64.github.io"), Some("localhost:5181"), 5181, false).is_some());
        assert!(forbidden(None, Some("evil.com:5181"), 5181, false).is_some());
        assert!(forbidden(None, Some("localhost:80"), 5181, false).is_some());
        assert!(forbidden(None, None, 5181, false).is_some());
        assert!(forbidden(None, Some("192.168.1.5:5181"), 5181, false).is_some());
    }

    #[test]
    fn public_allows_ip_hosts_only() {
        assert_eq!(forbidden(Some("http://192.168.1.5:5181"), Some("192.168.1.5:5181"), 5181, true), None);
        assert!(forbidden(None, Some("evil.com:5181"), 5181, true).is_some());
        assert!(forbidden(Some("http://evil.com"), Some("192.168.1.5:5181"), 5181, true).is_some());
    }
}
