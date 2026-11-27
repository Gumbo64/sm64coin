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
//!
//! In-process steps (`--in-process`, [`enable_inproc`]: the Android app) have no child and no pipes: the step's hooks call
//! [`frame`] / [`viz_on`] / [`requested`] / [`exchange`] in the host's own process, [`input`] writes the same mailbox the
//! hooks read, and the frames go to a [`Sink`] (the host's channel; by default a line on stdout as the parent would forward it).
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

pub const MARK: &str = "\u{1}VIZ";

static ENABLED: AtomicBool = AtomicBool::new(false);
static INPROC: AtomicBool = AtomicBool::new(false);
/// Where in-process frames go: (kind, key, payload), kind one of [`KINDS`].
pub type Sink = Arc<dyn Fn(&str, &str, &str) + Send + Sync>;
static SINK: Mutex<Option<Sink>> = Mutex::new(None);
/// Frames the running step has simulated, as its hook last reported (in-process: the host's frames/s counter).
static STEP_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Turns the feed on for steps that run in this process: `on` is its initial state, `sink` takes the frames (None: stdout
/// lines `MARK 0 kind key payload`, what automine forwards for a child's).
pub fn enable_inproc(on: bool, sink: Option<Sink>) {
    ENABLED.store(true, Ordering::Relaxed);
    INPROC.store(true, Ordering::Relaxed);
    *SINK.lock().unwrap() = sink;
    INPUT.lock().unwrap().viz_on = on;
}

/// The host stopped: frames go nowhere (the sink is dropped), the feed is off.
pub fn disable_inproc() {
    *SINK.lock().unwrap() = None;
    INPUT.lock().unwrap().viz_on = false;
}

/// A new in-process step begins: its mailbox commands count as not yet given, its frame counter restarts.
pub fn step_begin() {
    INPUT.lock().unwrap().given.clear();
    STEP_FRAMES.store(0, Ordering::Relaxed);
}

pub fn step_frames() -> u64 {
    STEP_FRAMES.load(Ordering::Relaxed)
}

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

/// The mailbox keys the search reads (the desktop app checks each one's schema, desktop/src-tauri/src/vizfeed.rs).
const MAIL_KEYS: &[&str] = &["goal", "cmds", "reqs"];
/// The browser's mailbox is 2 MB (web/src/lib/webminer/board.js); a line over this is ignored.
const MAX_INPUT: usize = 4 << 20;

/// A line from the miner's own stdin (the desktop app): `{"viz_on": bool}` or `{"mail": {...}}`. Only those two shapes
/// (and only the known mailbox keys) pass, re-serialised, to the step's child.
pub fn input(line: &str) {
    if line.len() > MAX_INPUT {
        return;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
    if INPROC.load(Ordering::Relaxed) {
        let mut i = INPUT.lock().unwrap();
        if let Some(b) = v.get("viz_on").and_then(|b| b.as_bool()) {
            i.viz_on = b;
        }
        if let Some(m) = v.get("mail").and_then(|m| m.as_object()) {
            for (k, x) in m.iter().filter(|(k, _)| MAIL_KEYS.contains(&k.as_str())) {
                i.mail.insert(k.clone(), x.clone());
            }
        }
        return;
    }
    let mut l = LINK.lock().unwrap();
    if let Some(b) = v.get("viz_on").and_then(|b| b.as_bool()) {
        l.viz_on = b;
        send(&mut l, &serde_json::json!({ "viz_on": b }).to_string());
    }
    if let Some(m) = v.get("mail").and_then(|m| m.as_object()) {
        let m: serde_json::Map<String, serde_json::Value> = m.iter().filter(|(k, _)| MAIL_KEYS.contains(&k.as_str())).map(|(k, x)| (k.clone(), x.clone())).collect();
        for (k, x) in &m {
            l.mail.insert(k.clone(), x.clone());
        }
        send(&mut l, &serde_json::json!({ "mail": m }).to_string());
    }
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
/// A frame is `MARK TAB kind TAB key TAB payload` with one of these kinds (child.rs `viz`, [`exchange`]); a line that merely
/// starts with the mark is the step's log.
const KINDS: &[&str] = &["viz", "state", "edges", "pert", "path", "hybrid_out"];

pub fn child_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix(MARK).and_then(|r| r.strip_prefix('\t')) else { return false };
    let mut p = rest.splitn(3, '\t');
    let (Some(kind), Some(_key), Some(_payload)) = (p.next(), p.next(), p.next()) else { return false };
    if !KINDS.contains(&kind) {
        return false;
    }
    let out = std::io::stdout();
    let _ = writeln!(out.lock(), "{MARK}\t0\t{rest}");
    true
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

/// The step's channel to the viz mailbox (hybrid commands in, `hybrid_out` frames out): the `exchange` of a step's
/// [`super::child::StepIo`] when the feed is on.
pub fn exchange(kind: &str, payload: &str) -> Option<String> {
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
    if INPROC.load(Ordering::Relaxed) {
        let sink = SINK.lock().unwrap().clone();
        match sink {
            Some(f) => f(kind, key, payload),
            None => {
                let out = std::io::stdout();
                let _ = writeln!(out.lock(), "{MARK}\t0\t{kind}\t{key}\t{payload}");
            }
        }
        return;
    }
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
    STEP_FRAMES.store(frames, Ordering::Relaxed);
    if let Some(p) = PROGRESS.get() {
        let _ = std::fs::write(p, frames.to_string());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(super::b64(b""), "");
        assert_eq!(super::b64(b"f"), "Zg==");
        assert_eq!(super::b64(b"fo"), "Zm8=");
        assert_eq!(super::b64(b"foo"), "Zm9v");
        assert_eq!(super::b64(&[0xff, 0xfe, 0x00, 0x7f]), "//4Afw==");
    }

    #[test]
    fn mail_state_is_replayed_to_a_new_child() {
        super::input(r#"{"viz_on":false}"#);
        super::input(r#"{"mail":{"goal":{"level":9}}}"#);
        super::input(r#"{"mail":{"cmds":[]}}"#);
        let l = super::LINK.lock().unwrap();
        assert!(!l.viz_on);
        assert_eq!(l.mail.get("goal").unwrap()["level"], 9);
        assert!(l.mail.contains_key("cmds"));
        drop(l);
        super::input(r#"{"mail":{"evil":1}}"#);
        assert!(!super::LINK.lock().unwrap().mail.contains_key("evil"));
    }

    #[test]
    fn only_well_formed_frames_are_forwarded() {
        assert!(super::child_line("\u{1}VIZ\tviz\t\t{}"));
        assert!(super::child_line("\u{1}VIZ\tstate\tk\tZm9v"));
        assert!(!super::child_line("\u{1}VIZ"));
        assert!(!super::child_line("\u{1}VIZnot a frame"));
        assert!(!super::child_line("\u{1}VIZ\tother\tk\tx"));
        assert!(!super::child_line("\u{1}VIZ\tviz\tk"));
        assert!(!super::child_line("root: frame 0"));
    }
}
