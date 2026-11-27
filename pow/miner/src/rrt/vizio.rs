//! The native miner's side channel for the website's visualisations (the browser build calls `sm64miner.viz*` in the page
//! instead; see child.rs `viz`). Off unless automine runs with `--viz`; then, per step:
//!
//! * child -> parent: lines on the child's stdout that start with [`MARK`]: `MARK kind TAB key TAB payload`, where kind is
//!   `viz` (the sampler's JSON line), `state` / `edges` (bytes, base64), `pert` / `hybrid_out` (JSON), `path` (`id` + flat pads
//!   as JSON). The parent ([`child_line`]) forwards each on its own stdout as `MARK worker TAB kind TAB key TAB payload`
//!   (worker 0: one campaign) and logs every other line of the child as before.
//! * parent -> child: JSON lines on the child's stdin: `{"viz_on": bool}` and `{"mail": {goal, cmds, reqs}}` (the page's
//!   mailbox: the sampling bubble, hybrid commands, node requests; keys replace). The parent ([`input`]) keeps the latest of
//!   each and replays them to every new step's child.
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

pub const MARK: &str = "\u{1}VIZ";

static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

// ---------------------------------------------------------------- parent (automine)

struct Link {
    viz_on: bool,
    mail: serde_json::Map<String, serde_json::Value>,
    child: Option<std::process::ChildStdin>,
}

static LINK: LazyLock<Mutex<Link>> = LazyLock::new(|| Mutex::new(Link { viz_on: true, mail: serde_json::Map::new(), child: None }));

/// Turns the channel on (automine `--viz`): `on` is the feed's initial state.
pub fn enable(on: bool) {
    ENABLED.store(true, Ordering::Relaxed);
    LINK.lock().unwrap().viz_on = on;
}

fn send(link: &mut Link, line: &str) {
    if let Some(c) = link.child.as_mut() {
        if writeln!(c, "{line}").and_then(|_| c.flush()).is_err() {
            link.child = None;
        }
    }
}

/// A line from the miner's own stdin (the desktop app): `{"viz_on": bool}` or `{"mail": {...}}`.
pub fn input(line: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
    let mut l = LINK.lock().unwrap();
    if let Some(b) = v.get("viz_on").and_then(|b| b.as_bool()) {
        l.viz_on = b;
    }
    if let Some(m) = v.get("mail").and_then(|m| m.as_object()) {
        for (k, x) in m {
            l.mail.insert(k.clone(), x.clone());
        }
    }
    send(&mut l, line);
}

/// A new step's child: its stdin, and the current state replayed to it.
pub fn attach(mut stdin: std::process::ChildStdin) {
    let mut l = LINK.lock().unwrap();
    let hello = format!("{}\n{}", serde_json::json!({ "viz_on": l.viz_on }), serde_json::json!({ "mail": l.mail }));
    if writeln!(stdin, "{hello}").and_then(|_| stdin.flush()).is_ok() {
        l.child = Some(stdin);
    }
}

pub fn detach() {
    LINK.lock().unwrap().child = None;
}

/// A line of a step child's stdout: a viz frame is forwarded (true), anything else is the step's log (false).
pub fn child_line(line: &str) -> bool {
    match line.strip_prefix(MARK) {
        Some(rest) => {
            let out = std::io::stdout();
            let _ = writeln!(out.lock(), "{MARK}\t0{rest}");
            true
        }
        None => false,
    }
}

// ---------------------------------------------------------------- child (rrtrun)

struct Input {
    viz_on: bool,
    mail: serde_json::Map<String, serde_json::Value>,
    given: std::collections::HashSet<u64>,
}

static INPUT: LazyLock<Mutex<Input>> = LazyLock::new(|| Mutex::new(Input { viz_on: false, mail: serde_json::Map::new(), given: std::collections::HashSet::new() }));
static PROGRESS: OnceLock<String> = OnceLock::new();

/// In the step's process (job.viz): reads the parent's stdin, answers the search's `hybrid` exchange from the mailbox.
pub fn child_start(progress_file: String) {
    ENABLED.store(true, Ordering::Relaxed);
    let _ = PROGRESS.set(progress_file);
    std::thread::spawn(|| {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            let mut i = INPUT.lock().unwrap();
            if let Some(b) = v.get("viz_on").and_then(|b| b.as_bool()) {
                i.viz_on = b;
            }
            if let Some(m) = v.get("mail").and_then(|m| m.as_object()) {
                for (k, x) in m {
                    i.mail.insert(k.clone(), x.clone());
                }
            }
        }
        // The parent closed the pipe: no more input (it dies with us anyway).
    });
    let _ = super::child::EXCHANGE.set(std::sync::Arc::new(exchange));
}

fn exchange(kind: &str, payload: &str) -> Option<String> {
    match kind {
        // The page's mailbox for this step (board.js makeHybridPoll): the bubble and the commands not given yet.
        "hybrid" => {
            let p: serde_json::Value = serde_json::from_str(payload).ok()?;
            let mut i = INPUT.lock().unwrap();
            let cmds = i.mail.get("cmds").and_then(|c| c.as_array()).cloned().unwrap_or_default();
            let mut out = Vec::new();
            for c in cmds {
                let Some(id) = c.get("id").and_then(|x| x.as_u64()) else { continue };
                if i.given.contains(&id) {
                    continue;
                }
                let mine = match c.get("kind").and_then(|k| k.as_str()) {
                    Some("state") => c.get("vkey") == p.get("vkey"),
                    Some("insert") => c.get("tag") == p.get("tag"),
                    _ => false,
                };
                if mine {
                    i.given.insert(id);
                    out.push(c);
                }
            }
            Some(serde_json::json!({ "goal": i.mail.get("goal").cloned().unwrap_or(serde_json::Value::Null), "cmds": out }).to_string())
        }
        "hybrid_out" => {
            frame("hybrid_out", "", payload);
            None
        }
        _ => None,
    }
}

pub fn frame(kind: &str, key: &str, payload: &str) {
    let out = std::io::stdout();
    let _ = writeln!(out.lock(), "{MARK}\t{kind}\t{key}\t{payload}");
}

pub fn viz_on() -> bool {
    INPUT.lock().unwrap().viz_on
}

/// The node ids the page asked for in step `key` (once each).
pub fn requested(key: &str) -> Vec<u32> {
    let mut i = INPUT.lock().unwrap();
    let reqs = i.mail.get("reqs").and_then(|r| r.as_array()).cloned().unwrap_or_default();
    let mut ids = Vec::new();
    for r in reqs {
        let (Some(id), Some(nid)) = (r.get("id").and_then(|x| x.as_u64()), r.get("nid").and_then(|x| x.as_u64())) else { continue };
        let gid = id | 1 << 40;
        if r.get("key").and_then(|k| k.as_str()) != Some(key) || i.given.contains(&gid) {
            continue;
        }
        i.given.insert(gid);
        ids.push(nid as u32);
    }
    ids
}

pub fn progress(frames: u64) {
    if let Some(p) = PROGRESS.get() {
        let _ = std::fs::write(p, frames.to_string());
    }
}
