//! The native miner: `sm64coin-automine` as a low-priority child against the local node. Its output is
//! streamed to the webview as `miner-log` events (one line each); `miner-state` fires on start and exit.
use crate::{AppError, Dirs};
use serde::Serialize;
use sm64coin_launcher::proc::Proc;
use sm64coin_launcher::util::lower_priority;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Command;
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
}

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
    pub fn start(app: &AppHandle, d: &Dirs, rom: &Path, rpc_port: u16, address: &str, threads: Option<usize>) -> Result<Miner, AppError> {
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
        let route = d.bundle.join("routes").join("block1-human.m64");
        if route.is_file() {
            cmd.arg("--route").arg(route);
        }
        let log = log_dir.join("automine.log");
        let (proc, out, err) = Proc::spawn_piped("miner", cmd, log.clone()).map_err(|e| AppError::new("miner_start", e))?;
        lower_priority(&proc.child);
        let shared = Arc::new(Mutex::new(Shared::default()));
        let file = Arc::new(Mutex::new(std::fs::File::create(&log).ok()));
        let started = Instant::now();
        let _ = started;
        let mut readers = Vec::new();
        for r in [Box::new(out) as Box<dyn std::io::Read + Send>, Box::new(err)] {
            let (shared, file, app) = (shared.clone(), file.clone(), app.clone());
            readers.push(std::thread::spawn(move || {
                for line in BufReader::new(r).lines().map_while(Result::ok) {
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
        let m = Miner { proc, shared, address: address.to_string(), threads, started_ms: now_ms() };
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

    pub fn stop(&mut self) {
        self.proc.stop(15);
    }
}

impl Drop for Miner {
    fn drop(&mut self) {
        self.stop();
    }
}
