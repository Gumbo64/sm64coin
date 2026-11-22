mod crypto;
mod scan;
mod share;

use clap::Parser;
use scan::{Participant, State};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "sm64coin-leaderboard", about = "Opt-in SM64Coin mining leaderboard")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:16481")]
    daemon: String,
    #[arg(long, default_value = "127.0.0.1:16490")]
    bind: String,
    #[arg(long, default_value = "leaderboard-data")]
    data_dir: String,
    /// Seconds between chain polls.
    #[arg(long, default_value_t = 10)]
    poll_secs: u64,
    /// Another leaderboard server to share with (URL or host:port), repeatable
    #[arg(long = "peer-leaderboard", value_name = "URL")]
    peer_leaderboard: Vec<String>,
    /// File of peer leaderboard URLs, one per line, repeatable
    #[arg(long = "peers-file", value_name = "FILE")]
    peers_file: Vec<String>,
    /// Seconds between pulls from peers (jittered +-20%)
    #[arg(long, default_value_t = 300)]
    share_secs: u64,
    /// Do not look for peers among the node's connected peers
    #[arg(long)]
    no_discover: bool,
    /// Leaderboard port tried on the node's connected peers
    #[arg(long, default_value_t = 16490)]
    peer_port: u16,
    /// Public URL of this server, sent to peers we pull from so they can pull from us too
    #[arg(long, value_name = "URL")]
    announce_url: Option<String>,
}

fn respond(req: tiny_http::Request, code: u16, body: Value) {
    let h = |k: &str, v: &str| tiny_http::Header::from_bytes(k, v).unwrap();
    let r = tiny_http::Response::from_string(body.to_string())
        .with_status_code(code)
        .with_header(h("Content-Type", "application/json"))
        .with_header(h("Access-Control-Allow-Origin", "*"))
        .with_header(h("Access-Control-Allow-Headers", "content-type"));
    let _ = req.respond(r);
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.data_dir)?;
    let path = std::path::Path::new(&args.data_dir).join("leaderboard.json");
    let state = Arc::new(Mutex::new(State::load(&path, &args.daemon)));
    {
        let s = state.clone();
        let poll = args.poll_secs;
        std::thread::spawn(move || loop {
            if let Err(e) = s.lock().unwrap().sync() {
                eprintln!("sync error: {e}");
            }
            std::thread::sleep(Duration::from_secs(poll));
        });
    }
    let mut rl = share::RateLimit::default();
    let peers = Arc::new(Mutex::new(share::Peers::default()));
    {
        let mut p = peers.lock().unwrap();
        let mut urls = args.peer_leaderboard.clone();
        for f in &args.peers_file {
            urls.extend(share::read_peers_file(std::path::Path::new(f)));
        }
        for u in urls {
            if !p.add(&u, "config") {
                eprintln!("ignoring peer {u}");
            }
        }
    }
    let id = format!("{:016x}{:08x}", scan::now() ^ ((std::process::id() as u64) << 32), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0));
    share::spawn(state.clone(), peers.clone(), share::Cfg {
        daemon: args.daemon.clone(), path: path.clone(), id: id.clone(), secs: args.share_secs.max(1),
        discover: !args.no_discover, peer_port: args.peer_port, announce: args.announce_url.clone(),
    });
    let server = tiny_http::Server::http(&args.bind).map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("leaderboard listening on http://{} (daemon {})", args.bind, args.daemon);
    for mut req in server.incoming_requests() {
        let method = req.method().to_string();
        let url = req.url().split('?').next().unwrap_or("").to_string();
        let mut body = String::new();
        if method == "POST" {
            use std::io::Read;
            let _ = req.as_reader().take(1 << 16).read_to_string(&mut body);
        }
        let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let field = |k: &str| parsed.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match (method.as_str(), url.as_str()) {
            ("OPTIONS", _) => respond(req, 204, json!({})),
            ("GET", "/health") => {
                let s = state.lock().unwrap();
                let p = peers.lock().unwrap();
                respond(req, 200, json!({"ok": true, "scanned_height": s.scanned_height(), "participants": s.participants.len(),
                    "peer_entries": s.peer_entries(), "peers": p.active(), "known_peers": p.map.len()}));
            }
            ("GET", "/peers") => respond(req, 200, peers.lock().unwrap().json()),
            ("GET", "/share") => {
                let remote = req.remote_addr().map(|a| a.ip());
                if let Some(ip) = remote {
                    if !rl.allow(ip, Duration::from_secs(2)) {
                        respond(req, 429, json!({"error": "slow down"}));
                        continue;
                    }
                    let ann = req.headers().iter().find(|h| h.field.equiv("X-Leaderboard-Url")).map(|h| h.value.to_string());
                    if let Some(u) = ann.and_then(|a| share::announce_ok(&a, ip)) {
                        peers.lock().unwrap().add(&u, "inbound");
                    }
                }
                let entries = state.lock().unwrap().share();
                respond(req, 200, json!({"version": 1, "id": id, "entries": entries}));
            }
            ("GET", "/leaderboard") => respond(req, 200, state.lock().unwrap().leaderboard()),
            ("POST", "/register") => {
                let r = state.lock().unwrap().register(&field("name"), &field("address"), &field("view_key"), &path);
                match r {
                    Ok(v) => respond(req, 200, v),
                    Err(e) => respond(req, 400, json!({"error": e})),
                }
            }
            ("POST", "/unregister") => {
                let r = state.lock().unwrap().unregister(&field("address"), &field("view_key"), &path);
                match r {
                    Ok(v) => respond(req, 200, v),
                    Err(e) => respond(req, 400, json!({"error": e})),
                }
            }
            _ => respond(req, 404, json!({"error": "not found"})),
        }
    }
    let _ = Participant::default;
    Ok(())
}
