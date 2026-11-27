//! The native miner: `sm64coin-automine` as a low-priority child against the local node. Its output is
//! streamed to the webview as `miner-log` events (one line each); `miner-state` fires on start and exit.
use crate::AppError;
use crate::desktop::Dirs;
use crate::vizfeed;
use serde::Serialize;
use sm64coin_launcher::proc::Proc;
use sm64coin_launcher::util::lower_priority;
use std::collections::VecDeque;
use std::io::{BufReader, Write};
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

const KEEP_LINES: usize = 300;

#[derive(Default)]
struct Shared {
    lines: VecDeque<String>,
    fps: f64,
    frames: u64,
    /// Blocks the node accepted from this miner.
    accepted: u32,
    exited: Option<Option<i32>>,
}

#[derive(Clone, Serialize)]
pub struct MinerInfo {
    pub running: bool,
    pub address: String,
    pub threads: usize,
    pub pid: Option<u32>,
    pub started_ms: u64,
    /// Emulated frames per second, all threads (from the miner's progress lines).
    pub fps: f64,
    pub frames: u64,
    pub accepted: u32,
    pub exit_code: Option<i32>,
    pub lines: Vec<String>,
}

impl MinerInfo {
    pub fn idle() -> Self {
        MinerInfo { running: false, address: String::new(), threads: 0, pid: None, started_ms: 0, fps: 0.0, frames: 0, accepted: 0, exit_code: None, lines: vec![] }
    }
}

pub struct Miner {
    proc: Proc,
    shared: Arc<Mutex<Shared>>,
    address: String,
    threads: usize,
    started_ms: u64,
    /// Lines for the miner's stdin (the visualisation feed's requests, `--viz`, pow/miner/src/rrt/vizio.rs), written by a
    /// thread of their own so that a command never blocks on a full pipe (and never while holding the app's miner lock).
    to_stdin: SyncSender<String>,
}

/// Lines waiting for the miner's stdin before `send` reports it busy.
const STDIN_QUEUE: usize = 16;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// A mainnet address is long ("SM6..."); the miner itself checks it against the node's network.
fn plausible_address(a: &str) -> bool {
    a.starts_with("SM6") && a.len() > 90 && a.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn field(line: &str, key: &str) -> Option<f64> {
    line.split_whitespace().find_map(|w| w.strip_prefix(key)).and_then(|v| v.parse().ok())
}

impl Miner {
    pub fn start(app: &AppHandle, d: &Dirs, rom: &Path, rpc_port: u16, address: &str, threads: Option<usize>, viz_on: bool) -> Result<Miner, AppError> {
        if !plausible_address(address) {
            return Err(AppError::new("bad_address", "enter your SM64... address (the wallet's primary address) first"));
        }
        let bin = d.tool("sm64coin-automine");
        if !bin.is_file() {
            return Err(AppError::new("miner_binary_missing", format!("{} is missing", bin.display())));
        }
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        let threads = threads.unwrap_or_else(|| cores.saturating_sub(1).max(1)).clamp(1, cores.max(1));
        let log_dir = d.data.join("logs");
        std::fs::create_dir_all(&log_dir).map_err(|e| AppError::new("io", e.to_string()))?;
        let mut cmd = Command::new(&bin);
        cmd.args(["--address", address, "--daemon", &format!("http://127.0.0.1:{rpc_port}")]).arg("--rom").arg(rom);
        // The campaign's scratch (its memory across blocks, its roadmap) lasts between runs.
        cmd.arg("--rrt-scratch").arg(d.data.join("miner"));
        cmd.arg("--rh-threads").arg(threads.to_string());
        cmd.arg("--viz");
        let log = log_dir.join("automine.log");
        let (proc, out, err, mut stdin) = Proc::spawn_piped_stdin("miner", cmd, log.clone()).map_err(|e| AppError::new("miner_start", e))?;
        let (to_stdin, lines) = std::sync::mpsc::sync_channel::<String>(STDIN_QUEUE);
        let _ = to_stdin.try_send(serde_json::json!({ "viz_on": viz_on }).to_string());
        std::thread::spawn(move || {
            for l in lines {
                if writeln!(stdin, "{l}").and_then(|_| stdin.flush()).is_err() {
                    break; // the miner exited
                }
            }
        });
        lower_priority(&proc.child);
        let shared = Arc::new(Mutex::new(Shared::default()));
        let file = Arc::new(Mutex::new(std::fs::File::create(&log).ok()));
        let started = Instant::now();
        let _ = started;
        let mut readers = Vec::new();
        for (frames, r) in [(true, Box::new(out) as Box<dyn std::io::Read + Send>), (false, Box::new(err))] {
            let (shared, file, app) = (shared.clone(), file.clone(), app.clone());
            readers.push(std::thread::spawn(move || {
                // Lines are read with a size cap and decoded lossily: the reader must keep draining the pipe whatever comes.
                let mut r = BufReader::with_capacity(1 << 16, r);
                let mut buf = Vec::new();
                let mut dropped = 0u64;
                while let Ok(Some(fits)) = vizfeed::read_line_capped(&mut r, &mut buf, vizfeed::MAX_LINE) {
                    let line = String::from_utf8_lossy(&buf);
                    if line.starts_with(&vizfeed::VIZ_MARK[..4]) {
                        // A visualisation frame (stdout only), never a log line: to the page as `miner-viz` {w, kind, key, data},
                        // bytes stay base64. A malformed or oversized one is dropped (counted in the log).
                        match vizfeed::parse_frame(&line).filter(|_| frames && fits) {
                            Some(f) => {
                                let _ = app.emit("miner-viz", &serde_json::json!({ "w": f.w, "kind": f.kind, "key": f.key, "data": f.data }));
                            }
                            None => {
                                dropped += 1;
                                if dropped.is_power_of_two() {
                                    if let Some(f) = file.lock().unwrap().as_mut() {
                                        let _ = writeln!(f, "(dropped {dropped} malformed visualisation frame(s))");
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    let line = if fits { line.into_owned() } else { format!("{line}... (line cut at {} bytes)", vizfeed::MAX_LINE) };
                    if let Some(f) = file.lock().unwrap().as_mut() {
                        let _ = writeln!(f, "{line}");
                    }
                    {
                        let mut s = shared.lock().unwrap();
                        if line.starts_with("progress ") {
                            if let Some(v) = field(&line, "fps=") {
                                s.fps = v;
                            }
                            if let Some(v) = field(&line, "frames=") {
                                s.frames = v as u64;
                            }
                        } else {
                            if line.contains("ACCEPTED") || line.contains("submitted") && !line.contains("failed") {
                                s.accepted += 1;
                            }
                            s.lines.push_back(line.clone());
                            while s.lines.len() > KEEP_LINES {
                                s.lines.pop_front();
                            }
                        }
                    }
                    let _ = app.emit("miner-log", &line);
                }
            }));
        }
        // When both pipes close the miner has exited: record it and tell the UI.
        {
            let (shared, app) = (shared.clone(), app.clone());
            std::thread::spawn(move || {
                for r in readers {
                    let _ = r.join();
                }
                shared.lock().unwrap().exited.get_or_insert(None);
                let _ = app.emit("miner-state", &serde_json::json!({"running": false}));
            });
        }
        let m = Miner { proc, shared, address: address.to_string(), threads, started_ms: now_ms(), to_stdin };
        let _ = app.emit("miner-state", &serde_json::json!({"running": true}));
        Ok(m)
    }

    pub fn alive(&mut self) -> bool {
        self.proc.alive()
    }

    pub fn info(&mut self) -> MinerInfo {
        let running = self.proc.alive();
        let exit_code = if running { None } else { self.proc.child.try_wait().ok().flatten().and_then(|s| s.code()) };
        let s = self.shared.lock().unwrap();
        MinerInfo {
            running,
            address: self.address.clone(),
            threads: self.threads,
            pid: Some(self.proc.child.id()),
            started_ms: self.started_ms,
            fps: if running { s.fps } else { 0.0 },
            frames: s.frames,
            accepted: s.accepted,
            exit_code,
            lines: s.lines.iter().cloned().collect(),
        }
    }

    /// The feed on or off (the search goes on).
    pub fn viz_on(&self, on: bool) -> Result<(), AppError> {
        self.send(serde_json::json!({ "viz_on": on }))
    }

    /// The page's mailbox, checked against the schema the search reads (vizfeed::clean_mail) before it goes down.
    pub fn mail(&self, mail: &serde_json::Map<String, serde_json::Value>) -> Result<(), AppError> {
        let m = vizfeed::clean_mail(mail).map_err(|e| AppError::new("bad_mail", e))?;
        self.send(serde_json::json!({ "mail": m }))
    }

    /// One JSON line for the miner's stdin; never blocks (the writer thread drains the queue).
    fn send(&self, v: serde_json::Value) -> Result<(), AppError> {
        match self.to_stdin.try_send(v.to_string()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(AppError::new("miner_busy", "the native miner is not reading its requests; try again")),
            Err(TrySendError::Disconnected(_)) => Err(AppError::new("miner_not_running", "the native miner has exited")),
        }
    }

    pub fn stop(&mut self) {
        self.proc.stop(15);
    }
}

impl Drop for Miner {
    fn drop(&mut self) {
        self.stop();
    }
}
