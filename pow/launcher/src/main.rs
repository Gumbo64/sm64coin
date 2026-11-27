//! `sm64coin`: the one-command launcher of the SM64Coin release bundle.
//!
//! 1. finds the ROM (checks its SHA-1) next to the program, in `~/.sm64coin/` or at a given path,
//! 2. starts the node (`sm64coind`) with the peers of `seeds.txt`, data in `~/.sm64coin/node`,
//! 3. serves the website (`web/`) locally and proxies `/json_rpc` to the node (and
//!    `/leaderboard-api` to a leaderboard server, if one is configured or started),
//! 4. opens the browser and the wallet app, pointed at that node,
//! 5. on Ctrl-C (or when the node dies) stops everything it started.

use clap::Parser;
use sm64coin_launcher::node::{Node, NodeConfig};
use sm64coin_launcher::proc::Proc;
use sm64coin_launcher::util::{EXE, home, lower_priority, pick_port, plain, version};
use sm64coin_launcher::{dht, rom, rpc};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

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
    /// Run the light-client service beside the node (iroh endpoint + relay on demand: phones, apps and browsers read the chain
    /// through it). `auto` (default) = on with --public. Needs the `lc` build feature.
    #[cfg(feature = "lc")]
    #[arg(long, value_enum, default_value = "auto", num_args = 0..=1, default_missing_value = "on", value_name = "on|off|auto")]
    lc: LcMode,
    /// Where the light-client relay listens (used when it turns on)
    #[cfg(feature = "lc")]
    #[arg(long, default_value = "0.0.0.0:443", value_name = "ADDR")]
    lc_relay_bind: std::net::SocketAddr,
    /// Known light-client servers to learn peers from (iroh:<id>[@relay] or https URL), repeatable
    #[cfg(feature = "lc")]
    #[arg(long = "lc-bootstrap", value_name = "SPEC")]
    lc_bootstrap: Vec<String>,
    /// Extra argument for sm64coind, repeatable
    #[arg(long = "daemon-arg", value_name = "ARG", allow_hyphen_values = true)]
    daemon_args: Vec<String>,
}

#[cfg(feature = "lc")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum LcMode {
    On,
    Off,
    /// On when the website is served publicly (`--public`).
    Auto,
}

/// The website's origin (GitHub Pages), allowed to call the local node's RPC.

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

/// Largest request body the proxies take (submit_block bodies are < ~600 KB of hex).
const MAX_BODY: usize = 1 << 20;

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
        if !rpc::method_of(&body).is_some_and(|m| rpc::allowed(&m)) {
            return respond_text(req, 403, "this RPC method is not available through the website");
        }
    }
    let method = req.method().as_str().to_string();
    let hv = |name: &str| req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string());
    let (ct_in, accept) = (hv("content-type"), hv("accept"));
    match rpc::forward(&method, base, rest, ct_in.as_deref(), accept.as_deref(), &body, Duration::from_secs(60)) {
        Ok((code, ct, data)) => {
            let _ = req.respond(tiny_http::Response::from_data(data).with_status_code(code).with_header(header("Content-Type", &ct)));
        }
        Err(e) => respond_text(req, 502, &e),
    }
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
    let rom_path = if std::env::var_os("SM64COIN_TEST_NO_ROM").is_some() { PathBuf::from("no-rom-test") } else { rom::find(o.rom.as_deref(), &exe_dir, &data_dir)? };
    println!("ROM: {} (SHA-1 ok)", rom_path.display());

    let daemon_bin = tool("sm64coind");
    
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
    let mut o = o;

    let result = (|| -> Result<(), String> {
        // --- node ---
        let mut cfg = NodeConfig::new(daemon_bin.clone(), rom_path.clone(), data_dir.clone());
        cfg.seed_files = match &o.seeds {
            Some(f) => vec![f.clone()],
            None => vec![exe_dir.join("seeds.txt"), data_dir.join("seeds.txt")],
        };
        cfg.peers = o.peers.clone();
        (cfg.solo, cfg.no_dht, cfg.dht_announce) = (o.solo, o.no_dht, o.dht_announce);
        (cfg.p2p_port, cfg.rpc_port, cfg.zmq_port) = (o.p2p_port, o.rpc_port, o.zmq_port);
        cfg.daemon_args = o.daemon_args.clone();
        let mut node = Node::start(&cfg)?;
        o.rpc_port = node.rpc_port;
        if let Some(p) = node.take_proc() {
            // The rest starts now, not when the node answers: the site and the wallet wait for it.
            procs.push(p);
            started_node = true;
        }
        let rpc_url = format!("http://127.0.0.1:{}", o.rpc_port);

        // --- light-client service (iroh) ---
        #[cfg(feature = "lc")]
        let _lc = if o.lc == LcMode::On || (o.lc == LcMode::Auto && o.public) {
            let mut c = sm64coin_launcher::lcserve::LcConfig::new(o.rpc_port, data_dir.join("lc"));
            c.relay_bind = o.lc_relay_bind;
            c.bootstrap = o.lc_bootstrap.clone();
            if o.no_dht {
                c.discovery = sm64coin_lc::endpoint::Discovery::N0;
            }
            match sm64coin_launcher::lcserve::start(c) {
                Ok(h) => Some(h),
                Err(e) => {
                    println!("(light-client service not started: {e})");
                    None
                }
            }
        } else {
            None
        };

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
                cmd.env("SM64COIN_ROM", &rom_path).env("SM64COIN_DATA_DIR", &data_dir);
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
                if rpc::get_info(o.rpc_port).is_some() {
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
            cmd.args(["--address", addr, "--daemon", &rpc_url]).arg("--rom").arg(&rom_path);
            // The campaign's scratch (step files, its memory across blocks, its roadmap) lasts
            // between runs; threads: all but one core.
            cmd.arg("--rrt-scratch").arg(data_dir.join("miner"));
            let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).saturating_sub(1).max(1);
            cmd.arg("--rh-threads").arg(threads.to_string());
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
            if let Some(i) = rpc::get_info(o.rpc_port) {
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
                let list = rpc::peers(o.rpc_port).unwrap_or_default();
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
