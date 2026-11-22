//! `sm64coin`: the one-command launcher of the SM64Coin release bundle.
//!
//! 1. finds the ROM (checks its SHA-1) next to the program, in `~/.sm64coin/` or at a given path,
//! 2. starts the node (`sm64coind`) with the peers of `seeds.txt`, data in `~/.sm64coin/node`,
//! 3. serves the website (`web/`) locally and proxies `/json_rpc` to the node (and
//!    `/leaderboard-api` to a leaderboard server, if one is configured or started),
//! 4. opens the browser and the wallet app, pointed at that node,
//! 5. on Ctrl-C (or when the node dies) stops everything it started.

use clap::Parser;
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

#[derive(Parser, Debug)]
#[command(
    name = "sm64coin",
    version,
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
    /// Proxy /leaderboard-api to this leaderboard server (e.g. http://host:16490)
    #[arg(long, value_name = "URL")]
    leaderboard_url: Option<String>,
    /// Also run the leaderboard server (sm64coin-leaderboard) on this port
    #[arg(long, value_name = "PORT")]
    leaderboard_port: Option<u16>,
    /// Also run the automated miner (sm64coin-automine), mining to this address
    #[arg(long, value_name = "SM64...")]
    mine: Option<String>,
    /// Extra argument for sm64coind, repeatable
    #[arg(long = "daemon-arg", value_name = "ARG", allow_hyphen_values = true)]
    daemon_args: Vec<String>,
}

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
}

fn header(k: &str, v: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn respond_text(req: tiny_http::Request, code: u16, body: &str) {
    let _ = req.respond(tiny_http::Response::from_string(body).with_status_code(code).with_header(header("Content-Type", "text/plain; charset=utf-8")));
}

fn proxy(mut req: tiny_http::Request, base: &str, rest: &str) {
    let mut body = Vec::new();
    let _ = req.as_reader().read_to_end(&mut body);
    let url = format!("{base}{rest}");
    let method = req.method().as_str().to_string();
    let mut r = ureq::request(&method, &url).timeout(Duration::from_secs(120));
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
    let _ = resp.into_reader().read_to_end(&mut data);
    let _ = req.respond(tiny_http::Response::from_data(data).with_status_code(code).with_header(header("Content-Type", &ct)));
}

fn handle(req: tiny_http::Request, cfg: &WebCfg) {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').map(|(p, q)| (p.to_string(), format!("?{q}"))).unwrap_or((url.clone(), String::new()));
    if path == "/json_rpc" {
        return proxy(req, &cfg.rpc, "/json_rpc");
    }
    if let Some(rest) = path.strip_prefix("/leaderboard-api") {
        return match &cfg.leaderboard {
            Some(base) => proxy(req, base, &format!("{}{}", if rest.is_empty() { "/" } else { rest }, query)),
            None => respond_text(req, 503, "no leaderboard configured (start sm64coin with --leaderboard-url or --leaderboard-port)"),
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
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_dir = plain(fs::canonicalize(&exe).unwrap_or(exe)).parent().map(Path::to_path_buf).unwrap_or_default();
    let data_dir = o.data_dir.clone().unwrap_or_else(|| home().join(".sm64coin"));
    let tool = |n: &str| -> PathBuf { exe_dir.join(format!("{n}{EXE}")) };

    println!("SM64Coin {}", env!("CARGO_PKG_VERSION"));
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
            peers.sort();
            peers.dedup();
            let mut cmd = Command::new(&daemon_bin);
            cmd.arg("--data-dir").arg(&node_dir).arg("--sm64-rom").arg(&rom);
            cmd.args(["--p2p-bind-port", &o.p2p_port.to_string(), "--rpc-bind-port", &o.rpc_port.to_string(), "--zmq-rpc-bind-port", &o.zmq_port.to_string()]);
            cmd.arg("--non-interactive").arg("--log-file").arg(log_dir.join("node-daemon.log"));
            if o.solo {
                cmd.arg("--sm64-solo");
            }
            for p in &peers {
                // In the peer list, and kept connected (a node alone on a new network has nothing else to find).
                cmd.args(["--add-peer", p, "--add-priority-node", p]);
            }
            cmd.args(&o.daemon_args);
            if peers.is_empty() && !o.solo {
                println!("Note: no peers are known (seeds.txt is empty). Pass --peer HOST:PORT to join a network.");
            }
            println!("Starting the node (data in {}) ...", node_dir.display());
            let mut node = Proc::spawn("node", cmd, log_dir.join("node.log"))?;
            node.stop_url = Some(format!("{rpc_url}/stop_daemon"));
            procs.push(node);
            started_node = true;
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

        // --- leaderboard ---
        let mut lb_url = o.leaderboard_url.clone().map(|u| u.trim_end_matches('/').to_string());
        if let Some(port) = o.leaderboard_port {
            let bin = tool("sm64coin-leaderboard");
            if !bin.is_file() {
                return Err(format!("--leaderboard-port: {} is not in this bundle", bin.display()));
            }
            let lb_data = data_dir.join("leaderboard");
            fs::create_dir_all(&lb_data).map_err(|e| e.to_string())?;
            let mut cmd = Command::new(bin);
            cmd.args(["--daemon", &rpc_url, "--bind", &format!("127.0.0.1:{port}"), "--poll-secs", "10"]).arg("--data-dir").arg(&lb_data);
            procs.push(Proc::spawn("leaderboard", cmd, log_dir.join("leaderboard.log"))?);
            lb_url = Some(format!("http://127.0.0.1:{port}"));
            println!("Leaderboard server on port {port}.");
        }

        // --- website ---
        let host = if o.public { "0.0.0.0" } else { "127.0.0.1" };
        let web_port = serve_web(host, o.web_port, WebCfg { dir: web_dir, rpc: rpc_url.clone(), leaderboard: lb_url })?;
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
                let cmd = Command::new(&gui);
                seed_wallet_settings(o.rpc_port);
                match Proc::spawn("wallet", cmd, log_dir.join("wallet-gui.log")) {
                    Ok(p) => {
                        println!("Wallet app started.");
                        procs.push(p);
                    }
                    Err(e) => println!("Could not start the wallet app: {e}"),
                }
            } else {
                println!("(no wallet app in this bundle; use sm64coin-wallet-cli)");
            }
        }

        // --- automated miner ---
        if let Some(addr) = &o.mine {
            let bin = tool("sm64coin-automine");
            if !bin.is_file() {
                return Err(format!("--mine: {} is not in this bundle", bin.display()));
            }
            let mut cmd = Command::new(bin);
            cmd.args(["--address", addr, "--daemon", &rpc_url]).arg("--rom").arg(&rom);
            let route = exe_dir.join("routes").join("block1-human.m64");
            if route.is_file() {
                cmd.arg("--route").arg(route);
            }
            procs.push(Proc::spawn("miner", cmd, log_dir.join("automine.log"))?);
            println!("Automated miner started, mining to {addr}.");
        }

        println!("\nLogs are in {}. Press Ctrl-C to stop.\n", log_dir.display());

        // --- watch ---
        let mut last_print = Instant::now() - Duration::from_secs(60);
        let mut last_h = (0u64, 0u64);
        let mut synced_said = false;
        loop {
            if stop.load(Ordering::SeqCst) {
                println!("\nShutting down ...");
                return Ok(());
            }
            if started_node && !procs[0].alive() {
                return Err(format!("the node stopped. Its log ({}):\n{}", procs[0].log.display(), procs[0].tail()));
            }
            if let Some(i) = rpc_get_info(o.rpc_port) {
                let h = i["height"].as_u64().unwrap_or(0);
                let target = i["target_height"].as_u64().unwrap_or(0).max(h);
                let synced = i["synchronized"].as_bool().unwrap_or(false);
                if synced && !synced_said {
                    println!("Node is synchronized at block {h}.");
                    synced_said = true;
                } else if !synced && ((h, target) != last_h && last_print.elapsed() > Duration::from_secs(10)) {
                    println!("Syncing: block {h} of {}", if target > h { target.to_string() } else { "?".into() });
                    last_print = Instant::now();
                    synced_said = false;
                }
                last_h = (h, target);
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
