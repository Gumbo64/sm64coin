//! The Android app's native miner: `sm64coin_miner::campaign::Campaign` in this process (no sidecar, no child process; the
//! search steps are threads). The Tauri commands in lib.rs are thin shims over [`MobileMiner`]; this module has no Tauri types
//! (events leave through an [`Emit`] closure) so the host's tests drive it the same way the commands do.
//!
//! Same names and shapes as the desktop miner (miner.rs), so the page's NativeMiner card and NativeFeed work unchanged:
//! events `miner-log` (a line), `miner-state` ({running}), `miner-viz` ({w, kind, key, data}); `MinerInfo` from status.
//!
//! The ROM is not a file the user picks here: the page sends its own (from its IndexedDB) once with `rom_put`, which checks the
//! SHA-1 and keeps it in the app's data dir. The daemon is the app's loopback proxy (walletproxy.rs: fails over between the public nodes; the command layer resolves it through
//! `remote::Remote`; the campaign never gets a URL the page chose).
use crate::vizfeed;
use crate::{AppError, Res};
use serde::Serialize;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use sm64coin_miner::campaign::{Campaign, CampaignConfig, Event};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The US ROM (.z64): its SHA-1 (pow/launcher rom.rs) and size.
pub const ROM_SHA1: &str = "9bef1128717f958171a4afac3ed78ee2bb4e86ce";
pub const ROM_SIZE: usize = 8 << 20;
const KEEP_LINES: usize = 300;

/// How events leave: `(event name, payload)`; in the app it is `AppHandle::emit`.
pub type Emit = Arc<dyn Fn(&str, Value) + Send + Sync>;

#[derive(Default)]
struct Shared {
    lines: VecDeque<String>,
    fps: f64,
    frames: u64,
    accepted: u32,
    exited: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct MinerInfo {
    pub running: bool,
    pub address: String,
    pub threads: usize,
    pub pid: Option<u32>,
    pub started_ms: u64,
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

struct Running {
    campaign: Arc<Campaign>,
    shared: Arc<Mutex<Shared>>,
    address: String,
    threads: usize,
    started_ms: u64,
}

pub struct MobileMiner {
    data: PathBuf,
    cur: Mutex<Option<Running>>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// A mainnet address is long ("SM6..."); the daemon checks it against its network.
fn plausible_address(a: &str) -> bool {
    a.starts_with("SM6") && a.len() > 90 && a.bytes().all(|b| b.is_ascii_alphanumeric())
}

impl MobileMiner {
    pub fn new(data: PathBuf) -> MobileMiner {
        MobileMiner { data, cur: Mutex::new(None) }
    }

    pub fn rom_path(&self) -> PathBuf {
        self.data.join("rom.z64")
    }

    /// Keeps the page's ROM: exactly the US ROM (size and SHA-1), written whole or not at all.
    pub fn rom_put(&self, bytes: &[u8]) -> Res<()> {
        if bytes.len() != ROM_SIZE {
            return Err(AppError::new("rom_wrong", format!("that is not the US Super Mario 64 ROM (.z64 is {ROM_SIZE} bytes, this is {})", bytes.len())));
        }
        let sha: String = Sha1::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
        if sha != ROM_SHA1 {
            return Err(AppError::new("rom_wrong", format!("that is not the US Super Mario 64 ROM (SHA-1 {ROM_SHA1}, this is {sha})")));
        }
        std::fs::create_dir_all(&self.data).map_err(|e| AppError::new("io", e.to_string()))?;
        let tmp = self.data.join("rom.z64.part");
        std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, self.rom_path())).map_err(|e| AppError::new("io", e.to_string()))
    }

    /// [`rom_put`](Self::rom_put) for the ROM as base64 text: Android's IPC carries no raw bodies (every invoke there is a
    /// JSON message, a `Uint8Array` would arrive as an array of 8 M numbers), so the page sends `{rom: "<base64>"}`. The
    /// length is checked before anything is decoded.
    pub fn rom_put_b64(&self, b64: &str) -> Res<()> {
        use base64::Engine;
        const B64_LEN: usize = ROM_SIZE.div_ceil(3) * 4;
        if b64.len() != B64_LEN {
            return Err(AppError::new("rom_wrong", format!("that is not the US Super Mario 64 ROM (.z64 is {ROM_SIZE} bytes, {B64_LEN} in base64; this is {})", b64.len())));
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| AppError::new("rom_wrong", format!("the ROM's base64 does not decode ({e})")))?;
        self.rom_put(&bytes)
    }

    pub fn has_rom(&self) -> bool {
        std::fs::metadata(self.rom_path()).is_ok_and(|m| m.len() == ROM_SIZE as u64)
    }

    /// Starts the campaign against `daemon` (already an allowed node, see lib.rs `allowed_daemon`). Blocking: loading the ROM
    /// compiles the game's WebAssembly (seconds on a phone), so the command runs it off the UI thread.
    pub fn start(&self, emit: Emit, daemon: &str, address: &str, threads: Option<usize>, viz_on: bool) -> Res<MinerInfo> {
        if !plausible_address(address) {
            return Err(AppError::new("bad_address", "enter your SM64... address (the wallet's primary address) first"));
        }
        let mut g = self.cur.lock().unwrap();
        if g.as_ref().is_some_and(|r| !r.shared.lock().unwrap().exited) {
            return Err(AppError::new("already_running", "the native miner is already running"));
        }
        *g = None; // a finished one (drops: joins its thread)
        if !self.has_rom() {
            return Err(AppError::new("rom_missing", "the app has no ROM yet (the page sends its own with rom_put)"));
        }
        let rom = std::fs::read(self.rom_path()).map_err(|e| AppError::new("rom_unreadable", e.to_string()))?;
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        let threads = threads.unwrap_or_else(|| cores.saturating_sub(1).clamp(1, 4)).clamp(1, cores.max(1));
        let mut cfg = CampaignConfig::new(daemon, address, rom, self.data.join("miner"));
        cfg.threads = threads;
        cfg.viz = true; // the feed exists; `viz_on` switches it (off it only counts frames), as on the desktop
        let campaign = Arc::new(Campaign::start(cfg).map_err(|e| AppError::new("miner_start", format!("{e:#}")))?);
        let rx = campaign.subscribe();
        campaign.mailbox(&json!({ "viz_on": viz_on }).to_string());
        let shared = Arc::new(Mutex::new(Shared::default()));
        {
            // One thread forwards the campaign's events and, every 2 s, the speed as the desktop's `progress` line.
            let (c, sh, emit) = (campaign.clone(), shared.clone(), emit.clone());
            std::thread::Builder::new()
                .name("miner-events".into())
                .spawn(move || {
                    let every = Duration::from_secs(2);
                    let mut last = std::time::Instant::now();
                    loop {
                        let ev = rx.recv_timeout(every.saturating_sub(last.elapsed()).max(Duration::from_millis(10)));
                        match ev {
                            Ok(Event::Log(l)) => {
                                let mut s = sh.lock().unwrap();
                                s.lines.push_back(l.clone());
                                while s.lines.len() > KEEP_LINES {
                                    s.lines.pop_front();
                                }
                                drop(s);
                                emit("miner-log", json!(l));
                            }
                            Ok(Event::Viz { kind, key, payload }) => emit("miner-viz", json!({ "w": 0, "kind": kind, "key": key, "data": payload })),
                            Ok(Event::Block(b)) => {
                                if b.accepted {
                                    sh.lock().unwrap().accepted += 1;
                                }
                            }
                            Ok(Event::State(_)) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                            Ok(Event::Stopped(e)) => {
                                if let Some(e) = e {
                                    let l = format!("native miner stopped: {e}");
                                    sh.lock().unwrap().lines.push_back(l.clone());
                                    emit("miner-log", json!(l));
                                }
                                sh.lock().unwrap().exited = true;
                                emit("miner-state", json!({ "running": false }));
                                return;
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                sh.lock().unwrap().exited = true;
                                emit("miner-state", json!({ "running": false }));
                                return;
                            }
                        }
                        if last.elapsed() >= every {
                            last = std::time::Instant::now();
                            let s = c.stats();
                            {
                                let mut g = sh.lock().unwrap();
                                g.fps = s.fps;
                                g.frames = s.frames;
                            }
                            emit("miner-log", json!(format!("progress frames={} fps={:.0}", s.frames, s.fps)));
                        }
                    }
                })
                .map_err(|e| AppError::new("miner_start", e.to_string()))?;
        }
        let r = Running { campaign, shared, address: address.to_string(), threads, started_ms: now_ms() };
        let info = Self::info_of(&r);
        *g = Some(r);
        emit("miner-state", json!({ "running": true }));
        Ok(info)
    }

    fn info_of(r: &Running) -> MinerInfo {
        let s = r.shared.lock().unwrap();
        let running = !s.exited;
        MinerInfo {
            running,
            address: r.address.clone(),
            threads: r.threads,
            pid: None,
            started_ms: r.started_ms,
            fps: if running { s.fps } else { 0.0 },
            frames: s.frames,
            accepted: s.accepted,
            exit_code: None,
            lines: s.lines.iter().cloned().collect(),
        }
    }

    pub fn status(&self) -> MinerInfo {
        self.cur.lock().unwrap().as_ref().map(Self::info_of).unwrap_or_else(MinerInfo::idle)
    }

    /// Stops and waits for the campaign (its running search ends at its next round). Blocking.
    pub fn stop(&self, emit: &Emit) -> MinerInfo {
        let r = self.cur.lock().unwrap().take();
        if let Some(r) = r {
            r.campaign.stop();
            r.campaign.join();
            let mut info = Self::info_of(&r);
            info.running = false;
            info.fps = 0.0;
            emit("miner-state", json!({ "running": false }));
            return info;
        }
        MinerInfo::idle()
    }

    fn mailbox(&self, line: Value) -> Res<()> {
        match self.cur.lock().unwrap().as_ref() {
            Some(r) if !r.shared.lock().unwrap().exited => {
                r.campaign.mailbox(&line.to_string());
                Ok(())
            }
            _ => Err(AppError::new("miner_not_running", "the native miner is not running")),
        }
    }

    /// The feed on or off (the search goes on).
    pub fn viz_on(&self, on: bool) -> Res<()> {
        self.mailbox(json!({ "viz_on": on }))
    }

    /// The page's mailbox, rebuilt from the schema the search reads (vizfeed::clean_mail) before it goes in.
    pub fn viz_mail(&self, mail: &serde_json::Map<String, Value>) -> Res<()> {
        let m = vizfeed::clean_mail(mail).map_err(|e| AppError::new("bad_mail", e))?;
        self.mailbox(json!({ "mail": m }))
    }
}

/// Host-side test of the Android miner core: the same calls the Tauri commands make (rom_put, start, status, viz_on, viz_mail,
/// stop), against an ISOLATED node that this test starts itself: `sm64coind --offline --sm64-solo` on a fresh data dir and
/// loopback ports, never networked, never the real chain. It needs the daemon and a ROM, so it skips (and says so) without them:
///   SM64COIND=<path of sm64coind>  SM64COIN_ROM=<US ROM .z64>  CARGO_PROFILE_DEV_OPT_LEVEL=3 cargo test --lib mobile_miner -- --nocapture
/// (optimised: an unoptimised search did not finish the first block in 20 minutes; optimised, 2 threads took 505 s)
/// (the defaults are the unpacked release bundle in ~/apps/sm64coin-*-linux-x86_64). `SM64COIN_TEST_SECS` bounds the wait for
/// the block (default 1200).
#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};

    // A mainnet-format address (the isolated node only checks the format and pays it in its own, private chain).
    const ADDR: &str = "SM64RtPqsHM63LZn2u9Myx9HtC4kjhu1h98SRtJz9USB7NZAoCMdZSBD7D1L2DWu9uCYZFUSVKytreuzEMJ6VjBU2tbjKqevFH";

    struct Node(Child);
    impl Drop for Node {
        fn drop(&mut self) {
            let _ = self.0.kill(); // only the child this test started
            let _ = self.0.wait();
        }
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn bundle_file(name: &str) -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        let mut dirs: Vec<_> = std::fs::read_dir(format!("{home}/apps")).ok()?.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("sm64coin-"))).collect();
        dirs.sort();
        dirs.into_iter().rev().map(|d| d.join(name)).find(|p| p.is_file())
    }

    fn rpc(port: u16, method: &str) -> Option<Value> {
        let r = ureq::post(&format!("http://127.0.0.1:{port}/json_rpc")).timeout(Duration::from_secs(5)).send_json(json!({"jsonrpc": "2.0", "id": 0, "method": method, "params": {}})).ok()?;
        r.into_json::<Value>().ok()?.get("result").cloned()
    }

    #[test]
    fn rom_put_checks_the_rom() {
        let dir = std::env::temp_dir().join(format!("sm64coin-mm-rom-{}", std::process::id()));
        let m = MobileMiner::new(dir.clone());
        assert_eq!(m.rom_put(&[0u8; 100]).unwrap_err().code, "rom_wrong");
        assert_eq!(m.rom_put(&vec![0u8; ROM_SIZE]).unwrap_err().code, "rom_wrong");
        assert_eq!(m.rom_put_b64("AAAA").unwrap_err().code, "rom_wrong");
        assert_eq!(m.rom_put_b64(&"!".repeat(ROM_SIZE.div_ceil(3) * 4)).unwrap_err().code, "rom_wrong");
        {
            use base64::Engine;
            assert_eq!(m.rom_put_b64(&base64::engine::general_purpose::STANDARD.encode(vec![0u8; ROM_SIZE])).unwrap_err().code, "rom_wrong");
        }
        assert!(!m.has_rom());
        let emit: Emit = Arc::new(|_, _| {});
        assert_eq!(m.start(emit.clone(), "http://127.0.0.1:1", "SM6short", None, false).unwrap_err().code, "bad_address");
        assert_eq!(m.start(emit, "http://127.0.0.1:1", ADDR, None, false).unwrap_err().code, "rom_missing");
        assert_eq!(m.viz_on(true).unwrap_err().code, "miner_not_running");
        assert!(!m.status().running);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mines_a_block_on_an_isolated_offline_node() {
        let (Some(daemon), Some(rom)) = (std::env::var_os("SM64COIND").map(PathBuf::from).or_else(|| bundle_file("sm64coind")), std::env::var_os("SM64COIN_ROM").map(PathBuf::from).or_else(|| bundle_file("baserom.us.z64"))) else {
            eprintln!("SKIPPED: set SM64COIND and SM64COIN_ROM (or unpack a release bundle in ~/apps)");
            return;
        };
        let rom_bytes = std::fs::read(&rom).expect("read the ROM");
        let dir = std::env::temp_dir().join(format!("sm64coin-mm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (rpc_port, p2p, zmq) = (free_port(), free_port(), free_port());
        let node = Node(
            Command::new(&daemon)
                .args(["--offline", "--sm64-solo", "--no-igd", "--non-interactive", "--data-dir"])
                .arg(dir.join("node"))
                .arg("--sm64-rom")
                .arg(&rom)
                .args(["--p2p-bind-ip", "127.0.0.1", "--rpc-bind-ip", "127.0.0.1", "--rpc-bind-port", &rpc_port.to_string(), "--p2p-bind-port", &p2p.to_string(), "--zmq-rpc-bind-port", &zmq.to_string(), "--log-file"])
                .arg(dir.join("node.log"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start sm64coind"),
        );
        let t0 = std::time::Instant::now();
        let start_height = loop {
            if let Some(h) = rpc(rpc_port, "get_info").and_then(|i| i["height"].as_u64()) {
                break h;
            }
            assert!(t0.elapsed() < Duration::from_secs(60), "the isolated node did not answer");
            std::thread::sleep(Duration::from_millis(300));
        };

        let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let emit: Emit = {
            let ev = events.clone();
            Arc::new(move |n, v| {
                let mut g = ev.lock().unwrap();
                if g.len() < 20_000 {
                    g.push((n.to_string(), v));
                }
            })
        };
        let m = MobileMiner::new(dir.join("app"));
        {
            use base64::Engine;
            m.rom_put_b64(&base64::engine::general_purpose::STANDARD.encode(&rom_bytes)).unwrap(); // as the Android page sends it
        }
        assert!(m.has_rom());
        let url = format!("http://127.0.0.1:{rpc_port}");
        assert!(url.starts_with("http://127.0.0.1:"), "this test mines only to its own loopback node");
        let info = m.start(emit.clone(), &url, ADDR, Some(2), true).expect("start");
        assert!(info.running && info.threads == 2);
        assert_eq!(m.start(emit.clone(), &url, ADDR, Some(2), true).unwrap_err().code, "already_running");
        m.viz_on(true).unwrap();
        m.viz_mail(&serde_json::Map::new()).unwrap();
        assert_eq!(m.viz_mail(json!({"nope": 1}).as_object().unwrap()).unwrap_err().code, "bad_mail");

        let limit = Duration::from_secs(std::env::var("SM64COIN_TEST_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(1200));
        let t1 = std::time::Instant::now();
        let mut height = start_height;
        while t1.elapsed() < limit {
            std::thread::sleep(Duration::from_secs(5));
            height = rpc(rpc_port, "get_info").and_then(|i| i["height"].as_u64()).unwrap_or(height);
            if height > start_height && m.status().accepted >= 1 {
                break;
            }
        }
        let st = m.status();
        let seen = |name: &str| events.lock().unwrap().iter().filter(|(n, _)| n == name).count();
        eprintln!("height {start_height} -> {height} after {:?}; accepted {}; fps {:.0}; frames {}; events: log {}, viz {}, state {}", t1.elapsed(), st.accepted, st.fps, st.frames, seen("miner-log"), seen("miner-viz"), seen("miner-state"));
        for l in st.lines.iter().rev().take(12).rev() {
            eprintln!("  | {l}");
        }
        let stopped = m.stop(&emit);
        assert!(!stopped.running && !m.status().running);
        drop(node);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(height > start_height, "the isolated node did not accept a block");
        assert!(st.accepted >= 1, "the miner did not report an accepted block");
        assert!(seen("miner-log") > 0 && seen("miner-state") >= 2);
        assert!(seen("miner-viz") > 0, "no visualisation frames reached the page");
        assert!(st.frames > 0);
    }
}
