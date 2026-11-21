mod crypto;
mod scan;

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
                respond(req, 200, json!({"ok": true, "scanned_height": s.scanned_height(), "participants": s.participants.len()}));
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
