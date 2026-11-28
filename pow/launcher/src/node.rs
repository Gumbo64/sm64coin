//! The node (`sm64coind`): ports, peers, start, status, stop. Used by the CLI launcher and the desktop app.
use crate::proc::Proc;
use crate::{dht, rpc, seeds, util};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// Websites allowed to call the node's RPC from a visitor's browser (comma separated, as monerod takes the list).
pub const SITE_ORIGIN: &str = "https://gumbo64.github.io,https://sm64coin.pages.dev";
/// The desktop app's webview origins (Tauri: tauri://localhost on Linux/macOS, http(s)://tauri.localhost on Windows).
pub const APP_ORIGINS: &str = "tauri://localhost,http://tauri.localhost,https://tauri.localhost";

#[derive(Clone, Debug)]
pub struct NodeConfig {
    /// The `sm64coind` executable.
    pub daemon_bin: PathBuf,
    pub rom: PathBuf,
    /// Data dir of the app: the node lives in `<data_dir>/node`, logs in `<data_dir>/logs`.
    pub data_dir: PathBuf,
    /// Seeds files (one host:port per line).
    pub seed_files: Vec<PathBuf>,
    /// Extra peers (host:port).
    pub peers: Vec<String>,
    pub solo: bool,
    pub no_dht: bool,
    pub dht_announce: bool,
    pub p2p_port: u16,
    /// `--p2p-external-port`: what peers are told when a VPN/router forwards another public port to `p2p_port` (0: the
    /// listening port is advertised, as without forwarding).
    pub p2p_external_port: u16,
    pub rpc_port: u16,
    pub zmq_port: u16,
    /// Never move the P2P port when it is taken: the user (or a VPN's port forward) chose that exact port.
    pub strict_p2p_port: bool,
    /// Extra arguments for sm64coind.
    pub daemon_args: Vec<String>,
    /// Reuse a node already answering on `rpc_port` (left running on stop) instead of starting one.
    pub reuse: bool,
    /// `--rpc-access-control-origins` (None: leave the default, the node refuses browsers).
    pub origins: Option<String>,
}

impl NodeConfig {
    pub fn new(daemon_bin: PathBuf, rom: PathBuf, data_dir: PathBuf) -> Self {
        NodeConfig {
            daemon_bin,
            rom,
            data_dir,
            seed_files: vec![],
            peers: vec![],
            solo: false,
            no_dht: false,
            dht_announce: false,
            p2p_port: 16480,
            p2p_external_port: 0,
            rpc_port: 16481,
            zmq_port: 16482,
            strict_p2p_port: false,
            daemon_args: vec![],
            reuse: true,
            origins: Some(SITE_ORIGIN.into()),
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct NodeStatus {
    /// We started it (false: reused, or not running).
    pub running: bool,
    pub reused: bool,
    pub rpc_port: u16,
    pub p2p_port: u16,
    /// What peers are told (0: the listening port).
    pub p2p_external_port: u16,
    pub zmq_port: u16,
    /// The RPC answers.
    pub answering: bool,
    pub height: u64,
    pub target_height: u64,
    pub synchronized: bool,
    pub peers: usize,
    pub incoming_peers: u64,
    pub outgoing_peers: u64,
}

pub struct Node {
    proc: Option<Proc>,
    pub rpc_port: u16,
    pub p2p_port: u16,
    pub p2p_external_port: u16,
    pub zmq_port: u16,
    pub reused: bool,
    pub log_dir: PathBuf,
}

impl Node {
    /// Starts the node (or reuses one that answers on the RPC port). Returns right away: use
    /// `wait_ready` to wait for the RPC. Free ports near the configured ones are picked.
    pub fn start(cfg: &NodeConfig) -> Result<Node, String> {
        let log_dir = cfg.data_dir.join("logs");
        let node_dir = cfg.data_dir.join("node");
        std::fs::create_dir_all(&node_dir).and_then(|_| std::fs::create_dir_all(&log_dir)).map_err(|e| format!("cannot create {}: {e}", cfg.data_dir.display()))?;
        if cfg.reuse && rpc::get_info(cfg.rpc_port).is_some() {
            println!("A node is already running on RPC port {}: using it (it is left running when you exit).", cfg.rpc_port);
            return Ok(Node { proc: None, rpc_port: cfg.rpc_port, p2p_port: cfg.p2p_port, p2p_external_port: cfg.p2p_external_port, zmq_port: cfg.zmq_port, reused: true, log_dir });
        }
        if !cfg.daemon_bin.is_file() {
            return Err(format!("{} is missing", cfg.daemon_bin.display()));
        }
        // The P2P port is what a VPN's port forward reaches: when the user chose it, fail instead of silently moving to
        // another port, or the forward would point nowhere. RPC and ZMQ only serve this machine and may move.
        let p2p = if cfg.strict_p2p_port {
            if !util::port_free(cfg.p2p_port, "0.0.0.0") {
                return Err(format!("P2P port {} is already in use. Choose another port (your VPN's port forward must match it).", cfg.p2p_port));
            }
            cfg.p2p_port
        } else {
            let p = util::pick_port(cfg.p2p_port, "0.0.0.0", &[]);
            if p != cfg.p2p_port {
                println!("P2P port {} is in use by another program: using {p}.", cfg.p2p_port);
            }
            p
        };
        let rpcp = {
            let p = util::pick_port(cfg.rpc_port, "127.0.0.1", &[p2p]);
            if p != cfg.rpc_port {
                println!("RPC port {} is in use by another program: using {p}.", cfg.rpc_port);
            }
            p
        };
        let zmq = {
            let p = util::pick_port(cfg.zmq_port, "127.0.0.1", &[p2p, rpcp]);
            if p != cfg.zmq_port {
                println!("ZMQ port {} is in use by another program: using {p}.", cfg.zmq_port);
            }
            p
        };
        let mut peers: Vec<String> = Vec::new();
        for f in &cfg.seed_files {
            peers.extend(seeds::read_seeds(f));
        }
        peers.extend(cfg.peers.iter().cloned());
        // Hostnames resolved here; one that doesn't resolve is skipped (the daemon refuses
        // to start on an --add-peer it can't resolve).
        let named = peers.clone();
        let mut peers: Vec<String> = peers.iter().filter_map(|p| seeds::resolve_peer(p)).collect();
        peers.sort();
        peers.dedup();
        if !named.is_empty() {
            let mut n = named.clone();
            n.dedup();
            println!("Joining through: {}", n.join(", "));
        }
        // Nodes announced in the Mainline DHT (dht.rs): plain --add-peer, the node's own peer exchange does the rest.
        let mut dht_peers: Vec<String> = Vec::new();
        if !cfg.no_dht {
            use std::io::Write;
            print!("Looking for nodes in the DHT ... ");
            let _ = std::io::stdout().flush();
            dht_peers = dht::lookup(Duration::from_secs(20), 24).iter().map(|a| a.to_string()).filter(|a| !peers.contains(a)).collect();
            println!("{} found.", dht_peers.len());
        }
        let mut cmd = Command::new(&cfg.daemon_bin);
        cmd.arg("--data-dir").arg(&node_dir).arg("--sm64-rom").arg(&cfg.rom);
        cmd.args(["--p2p-bind-port", &p2p.to_string(), "--rpc-bind-port", &rpcp.to_string(), "--zmq-rpc-bind-port", &zmq.to_string()]);
        if cfg.p2p_external_port > 0 {
            // A VPN or router forwards this public port to `p2p`; peers must be told the public one.
            cmd.args(["--p2p-external-port", &cfg.p2p_external_port.to_string()]);
        }
        if let Some(o) = &cfg.origins {
            if !cfg.daemon_args.iter().any(|x| x.starts_with("--rpc-access-control-origins")) {
                cmd.args(["--rpc-access-control-origins", o]);
            }
        }
        // Monero allows one inbound connection per IP; with few seeds that locks out a second node
        // behind the same home router or carrier NAT. A flag, not a code change.
        if !cfg.daemon_args.iter().any(|x| x.starts_with("--max-connections-per-ip")) {
            cmd.arg("--max-connections-per-ip=16");
        }
        cmd.arg("--non-interactive").arg("--log-file").arg(log_dir.join("node-daemon.log"));
        if cfg.solo {
            cmd.arg("--sm64-solo");
        }
        for p in &peers {
            // In the peer list, and kept connected (a node alone on a new network has nothing else to find).
            cmd.args(["--add-peer", p, "--add-priority-node", p]);
        }
        for p in &dht_peers {
            cmd.args(["--add-peer", p]);
        }
        cmd.args(&cfg.daemon_args);
        if cfg.dht_announce {
            dht::announce_forever(p2p);
        }
        if peers.is_empty() && dht_peers.is_empty() && !cfg.solo {
            println!("Note: no seed node resolved and the DHT found none. Is this computer online? Pass --peer HOST:PORT to join through a node you know.");
        }
        println!("Starting the node (data in {}) ...", node_dir.display());
        let mut proc = Proc::spawn("node", cmd, log_dir.join("node.log"))?;
        proc.stop_url = Some(format!("http://127.0.0.1:{rpcp}/stop_daemon"));
        Ok(Node { proc: Some(proc), rpc_port: rpcp, p2p_port: p2p, p2p_external_port: cfg.p2p_external_port, zmq_port: zmq, reused: false, log_dir })
    }

    /// Did our node process exit? (false for a reused node)
    pub fn died(&mut self) -> bool {
        self.proc.as_mut().is_some_and(|p| !p.alive())
    }

    pub fn log_tail(&self) -> String {
        self.proc.as_ref().map(|p| format!("Its log ({}):\n{}", p.log.display(), p.tail())).unwrap_or_default()
    }

    /// Waits for the RPC to answer (a long chain can take minutes to load). `cancel` is polled.
    pub fn wait_ready(&mut self, timeout: Duration, cancel: &dyn Fn() -> bool) -> Result<(), String> {
        let t = Instant::now();
        loop {
            if cancel() {
                return Ok(());
            }
            if self.died() {
                return Err(format!("the node stopped right after starting. {}", self.log_tail()));
            }
            if rpc::get_info(self.rpc_port).is_some() {
                return Ok(());
            }
            if t.elapsed() > timeout {
                return Err(format!("the node did not start answering within {} s", timeout.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// The status that needs no network call: ownership and ports. The RPC probe is [`probe_status`], so a caller can drop
    /// the node lock before the (possibly slow) call.
    pub fn status_base(&mut self) -> NodeStatus {
        NodeStatus {
            running: self.proc.is_some() && !self.died(),
            reused: self.reused,
            rpc_port: self.rpc_port,
            p2p_port: self.p2p_port,
            p2p_external_port: self.p2p_external_port,
            zmq_port: self.zmq_port,
            ..Default::default()
        }
    }

    pub fn status(&mut self) -> NodeStatus {
        let mut s = self.status_base();
        probe_status(&mut s, self.rpc_port);
        s
    }

    /// Stops the node we started (a reused node is left alone).
    pub fn stop(&mut self) {
        if let Some(p) = self.proc.as_mut() {
            p.stop(120);
        }
        self.proc = None;
    }

    /// Hands the process over (the CLI keeps it in its own stop-in-reverse-order list).
    pub fn take_proc(&mut self) -> Option<Proc> {
        self.proc.take()
    }
}

/// Fills the RPC-derived fields of `s` with one short `get_info` call. The UI polls this while the node loads the chain, so it
/// must not take seconds; `get_info` already carries both connection counts, so no `get_connections` call is needed.
pub fn probe_status(s: &mut NodeStatus, rpc_port: u16) {
    if let Some(i) = rpc::get_info_timeout(rpc_port, Duration::from_millis(1200)) {
        s.answering = true;
        s.height = i["height"].as_u64().unwrap_or(0);
        s.target_height = i["target_height"].as_u64().unwrap_or(0).max(s.height);
        s.synchronized = i["synchronized"].as_bool().unwrap_or(false);
        s.incoming_peers = i["incoming_connections_count"].as_u64().unwrap_or(0);
        s.outgoing_peers = i["outgoing_connections_count"].as_u64().unwrap_or(0);
        s.peers = (s.incoming_peers + s.outgoing_peers) as usize;
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.stop();
    }
}
