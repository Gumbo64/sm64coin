//! The native miner's side channel for the website's visualisations (the browser build calls `sm64miner.viz*` in the page
//! instead; see child.rs `viz`). Everything lives in a [`Hub`]: one per campaign (so two campaigns of a process never share a
//! mailbox or a frame counter), or the step process's own ([`global`]) in `rrtrun`.
//!
//! * Steps on threads of the host (the default; [`crate::rrt::child::run_step_inproc`]): the step's hooks call
//!   [`Hub::frame`] / [`Hub::viz_on`] / [`Hub::requested`] / [`Hub::exchange`] directly, [`Hub::input`] writes the same
//!   mailbox the hooks read, and the frames go to the hub's [`Sink`] (the campaign's event channel).
//! * Steps in a child process (`automine --process`): child -> host: lines on the child's stdout that start with [`MARK`]:
//!   `MARK TAB kind TAB key TAB payload`, where kind is `viz` (the sampler's JSON line), `state` / `edges` (bytes, base64),
//!   `pert` / `hybrid_out` (JSON), `path` (`id` + flat pads as JSON). The host ([`Hub::child_line`]) hands each to the sink
//!   and logs every other line of the child as before. Host -> child: JSON lines on the child's stdin: `{"viz_on": bool}`
//!   and `{"mail": {goal, cmds, reqs}}` (the page's mailbox: the sampling bubble, hybrid commands, node requests; keys
//!   replace). The host ([`Hub::input`]) keeps the latest of each and replays them to every new step's child ([`Hub::attach`]).
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

pub const MARK: &str = "\u{1}VIZ";

/// Where a host's frames go: (kind, key, payload), kind one of [`KINDS`].
pub type Sink = Arc<dyn Fn(&str, &str, &str) + Send + Sync>;

/// The mailbox keys the search reads (the desktop app checks each one's schema, desktop/src-tauri/src/vizfeed.rs).
const MAIL_KEYS: &[&str] = &["goal", "cmds", "reqs"];
/// The browser's mailbox is 2 MB (web/src/lib/webminer/board.js); a line over this is ignored.
const MAX_INPUT: usize = 4 << 20;
/// A frame is `MARK TAB kind TAB key TAB payload` with one of these kinds (child.rs `viz`, [`Hub::exchange`]); a line that
/// merely starts with the mark is the step's log.
const KINDS: &[&str] = &["viz", "state", "edges", "pert", "path", "hybrid_out"];

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

// ---------------------------------------------------------------- hub

#[derive(Default)]
struct Input {
    viz_on: bool,
    mail: serde_json::Map<String, serde_json::Value>,
    /// Hybrid commands / node requests already handed to the running step.
    given: std::collections::HashSet<u64>,
}

pub struct Hub {
    /// false: the step process's own hub (frames go up its stdout raw, see [`Hub::frame`]).
    host: bool,
    enabled: AtomicBool,
    sink: Mutex<Option<Sink>>,
    input: Mutex<Input>,
    /// Process mode: the running step child's stdin.
    child: Mutex<Option<std::process::ChildStdin>>,
    /// Frames the running step has simulated, as its hook last reported (in-process).
    step_frames: AtomicU64,
    /// Frames simulated by this hub's steps so far (finished steps and the running one's last progress): the miner's speed
    /// is its rate.
    sim_frames: AtomicU64,
    progress_file: OnceLock<String>,
}

impl Hub {
    fn new(host: bool) -> Hub {
        Hub { host, enabled: AtomicBool::new(false), sink: Mutex::new(None), input: Mutex::new(Input::default()), child: Mutex::new(None), step_frames: AtomicU64::new(0), sim_frames: AtomicU64::new(0), progress_file: OnceLock::new() }
    }

    /// A host's hub (a campaign's): the feed is off until [`Hub::enable`].
    pub fn host() -> Arc<Hub> {
        Arc::new(Hub::new(true))
    }

    /// Turns the feed on for the steps of this hub: `on` is its initial state, `sink` takes the frames.
    pub fn enable(&self, on: bool, sink: Option<Sink>) {
        *self.sink.lock().unwrap() = sink;
        self.input.lock().unwrap().viz_on = on;
        self.enabled.store(true, Ordering::Relaxed);
    }

    /// The host stopped: frames go nowhere (the sink is dropped), the feed is off.
    pub fn disable(&self) {
        *self.sink.lock().unwrap() = None;
        self.input.lock().unwrap().viz_on = false;
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn sim_frames(&self) -> u64 {
        self.sim_frames.load(Ordering::Relaxed)
    }

    pub fn add_sim_frames(&self, n: u64) {
        self.sim_frames.fetch_add(n, Ordering::Relaxed);
    }

    /// A new in-process step begins: its mailbox commands count as not yet given, its frame counter restarts.
    pub fn step_begin(&self) {
        self.input.lock().unwrap().given.clear();
        self.step_frames.store(0, Ordering::Relaxed);
    }

    pub fn step_frames(&self) -> u64 {
        self.step_frames.load(Ordering::Relaxed)
    }

    /// A line from the miner's own stdin (the desktop app): `{"viz_on": bool}` or `{"mail": {...}}`. Only those two shapes
    /// (and only the known mailbox keys) pass; in process mode they are re-serialised to the step's child.
    pub fn input(&self, line: &str) {
        if line.len() > MAX_INPUT {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
        let mut i = self.input.lock().unwrap();
        let mut out = Vec::new();
        if let Some(b) = v.get("viz_on").and_then(|b| b.as_bool()) {
            i.viz_on = b;
            out.push(serde_json::json!({ "viz_on": b }).to_string());
        }
        if let Some(m) = v.get("mail").and_then(|m| m.as_object()) {
            let m: serde_json::Map<String, serde_json::Value> = m.iter().filter(|(k, _)| MAIL_KEYS.contains(&k.as_str())).map(|(k, x)| (k.clone(), x.clone())).collect();
            for (k, x) in &m {
                i.mail.insert(k.clone(), x.clone());
            }
            out.push(serde_json::json!({ "mail": m }).to_string());
        }
        drop(i);
        let mut c = self.child.lock().unwrap();
        if let Some(s) = c.as_mut() {
            if out.iter().try_for_each(|l| writeln!(s, "{l}").and_then(|_| s.flush())).is_err() {
                *c = None;
            }
        }
    }

    /// Process mode: a new step's child: its stdin, and the current state replayed to it.
    pub fn attach(&self, mut stdin: std::process::ChildStdin) {
        let i = self.input.lock().unwrap();
        let hello = format!("{}\n{}", serde_json::json!({ "viz_on": i.viz_on }), serde_json::json!({ "mail": i.mail }));
        drop(i);
        if writeln!(stdin, "{hello}").and_then(|_| stdin.flush()).is_ok() {
            *self.child.lock().unwrap() = Some(stdin);
        }
    }

    pub fn detach(&self) {
        *self.child.lock().unwrap() = None;
    }

    /// A line of a step child's stdout: a viz frame is handed to the sink (true), anything else is the step's log (false).
    pub fn child_line(&self, line: &str) -> bool {
        let Some(rest) = line.strip_prefix(MARK).and_then(|r| r.strip_prefix('\t')) else { return false };
        let mut p = rest.splitn(3, '\t');
        let (Some(kind), Some(key), Some(payload)) = (p.next(), p.next(), p.next()) else { return false };
        if !KINDS.contains(&kind) {
            return false;
        }
        self.emit(kind, key, payload);
        true
    }

    fn emit(&self, kind: &str, key: &str, payload: &str) {
        let sink = self.sink.lock().unwrap().clone();
        if let Some(f) = sink {
            f(kind, key, payload);
        }
    }

    // ------------------------------------------------------------ the step's side

    /// In the step's process (job.viz): reads the host's stdin, answers the search's `hybrid` exchange from the mailbox.
    pub fn child_start(self: &Arc<Self>, progress_file: String) {
        self.enabled.store(true, Ordering::Relaxed);
        let _ = self.progress_file.set(progress_file);
        let me = self.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                let mut i = me.input.lock().unwrap();
                if let Some(b) = v.get("viz_on").and_then(|b| b.as_bool()) {
                    i.viz_on = b;
                }
                if let Some(m) = v.get("mail").and_then(|m| m.as_object()) {
                    for (k, x) in m {
                        i.mail.insert(k.clone(), x.clone());
                    }
                }
            }
            // The host closed the pipe: no more input (it dies with us anyway).
        });
        let me = self.clone();
        let _ = super::child::EXCHANGE.set(Arc::new(move |k: &str, p: &str| me.exchange(k, p)));
    }

    /// The step's channel to the viz mailbox (hybrid commands in, `hybrid_out` frames out): the `exchange` of a step's
    /// [`super::child::StepIo`] when the feed is on.
    pub fn exchange(&self, kind: &str, payload: &str) -> Option<String> {
        match kind {
            // The page's mailbox for this step (board.js makeHybridPoll): the bubble and the commands not given yet.
            "hybrid" => {
                let p: serde_json::Value = serde_json::from_str(payload).ok()?;
                let mut i = self.input.lock().unwrap();
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
                self.frame("hybrid_out", "", payload);
                None
            }
            _ => None,
        }
    }

    pub fn frame(&self, kind: &str, key: &str, payload: &str) {
        if self.host {
            self.emit(kind, key, payload);
            return;
        }
        let out = std::io::stdout();
        let _ = writeln!(out.lock(), "{MARK}\t{kind}\t{key}\t{payload}");
    }

    pub fn viz_on(&self) -> bool {
        self.input.lock().unwrap().viz_on
    }

    /// The node ids the page asked for in step `key` (once each).
    pub fn requested(&self, key: &str) -> Vec<u32> {
        let mut i = self.input.lock().unwrap();
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

    pub fn progress(&self, frames: u64) {
        self.step_frames.store(frames, Ordering::Relaxed);
        if let Some(p) = self.progress_file.get() {
            let _ = std::fs::write(p, frames.to_string());
        }
    }
}

/// The hub of a step process (`rrtrun`): its frames go up its stdout, its mailbox is fed from its stdin.
pub fn global() -> &'static Arc<Hub> {
    static G: LazyLock<Arc<Hub>> = LazyLock::new(|| Arc::new(Hub::new(false)));
    &G
}

/// The hub of callers that bring none (`rrtbench`, tools): one for the process.
pub fn host_default() -> &'static Arc<Hub> {
    static G: LazyLock<Arc<Hub>> = LazyLock::new(Hub::host);
    &G
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(&[0xff, 0xfe, 0x00, 0x7f]), "//4Afw==");
    }

    #[test]
    fn mail_state_is_kept_per_hub_and_replayed() {
        let h = Hub::host();
        let other = Hub::host();
        h.input(r#"{"viz_on":false}"#);
        h.input(r#"{"mail":{"goal":{"level":9}}}"#);
        h.input(r#"{"mail":{"cmds":[]}}"#);
        let i = h.input.lock().unwrap();
        assert!(!i.viz_on);
        assert_eq!(i.mail.get("goal").unwrap()["level"], 9);
        assert!(i.mail.contains_key("cmds"));
        drop(i);
        h.input(r#"{"mail":{"evil":1}}"#);
        assert!(!h.input.lock().unwrap().mail.contains_key("evil"));
        // another campaign's hub saw none of it
        assert!(other.input.lock().unwrap().mail.is_empty());
    }

    #[test]
    fn only_well_formed_frames_reach_the_sink() {
        let h = Hub::host();
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        h.enable(true, Some(Arc::new(move |k: &str, key: &str, p: &str| g.lock().unwrap().push(format!("{k}|{key}|{p}")))));
        assert!(h.child_line("\u{1}VIZ\tviz\t\t{}"));
        assert!(h.child_line("\u{1}VIZ\tstate\tk\tZm9v"));
        assert!(!h.child_line("\u{1}VIZ"));
        assert!(!h.child_line("\u{1}VIZnot a frame"));
        assert!(!h.child_line("\u{1}VIZ\tother\tk\tx"));
        assert!(!h.child_line("\u{1}VIZ\tviz\tk"));
        assert!(!h.child_line("root: frame 0"));
        assert_eq!(*got.lock().unwrap(), vec!["viz||{}".to_string(), "state|k|Zm9v".to_string()]);
    }
}
