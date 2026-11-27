//! The app's light-client verification: on startup and on every new tip, `sm64coin_lc::verify::sync` checks the chain up to five
//! nodes serve from the checkpoint compiled into this release (header layer: links, ids, timestamps, difficulty, frame limit,
//! the miner's signature, proof weight, fork choice). The nodes on the verified best branch are the only ones the wallet proxy
//! and the native miner use (`Client::set_allowed`); a node that served an invalid chain is banned (a week). On desktop the
//! branch's current cycle is also replayed in the background (budgeted, cancelled by the next tip); Android does the header layer
//! only (no ROM-driven replay by default: battery).
//!
//! What the user sees: `Verification` (in `net_status`): `verified` ("verified (n nodes agree)"), `conflict` (nodes served
//! different valid chains or an invalid one; the heaviest valid branch is followed) or `unverified` (no node could be checked).
use serde::Serialize;
use sm64coin_lc::client::Client;
use sm64coin_lc::consensus::Sm64Consensus;
use sm64coin_lc::verify::{Checkpoint, Level, CrossCheckOptions, NodeVerdict, Outcome, Resume, VerifyError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// How the background replay of the current cycle stands.
#[derive(Clone, Debug, Serialize)]
pub struct ReplayStatus {
    /// `off` (not on this platform / no ROM), `running`, `valid`, `partial`, `invalid`, `unavailable`.
    pub state: &'static str,
    pub done: usize,
    pub total: usize,
    pub detail: String,
}

impl Default for ReplayStatus {
    fn default() -> Self {
        ReplayStatus { state: "off", done: 0, total: 0, detail: String::new() }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Verification {
    /// `checking` (first run in progress), `verified`, `conflict` or `unverified`.
    pub state: &'static str,
    /// The words for the status light and the Network page: "verified (3 nodes agree)" ...
    pub label: String,
    /// Nodes on the verified chain (at the tip or just behind it).
    pub agree: usize,
    pub asked: usize,
    /// At least three nodes agree on the tip.
    pub quorum: bool,
    pub height: Option<u64>,
    pub tip: Option<String>,
    /// `full` (rules from sm64pow) or `structural`.
    pub level: &'static str,
    pub checkpoint_height: u64,
    /// Nodes banned by this run (served an invalid chain).
    pub banned: Vec<String>,
    /// Nodes that served a different valid chain, or were unreachable (key and why).
    pub disagree: Vec<(String, String)>,
    pub error: Option<String>,
    pub replay: ReplayStatus,
    pub checked_at_ms: u64,
}

impl Default for Verification {
    fn default() -> Self {
        Verification { state: "checking", label: "checking the chain".into(), agree: 0, asked: 0, quorum: false, height: None, tip: None, level: "full", checkpoint_height: 0, banned: vec![], disagree: vec![], error: None, replay: ReplayStatus::default(), checked_at_ms: 0 }
    }
}

/// The anchor: the release's compiled-in checkpoint for mainnet; for another chain (`SM64COIN_GENESIS`: tests, closed networks)
/// its genesis block.
pub fn checkpoint_for(genesis: &[u8; 32]) -> Checkpoint {
    let mainnet = sm64coin_lc::checkpoints::mainnet().first().is_some_and(|c| c.height == 0 && c.id == *genesis);
    if mainnet { sm64coin_lc::checkpoints::mainnet_latest() } else { Checkpoint::genesis(*genesis) }
}

/// Shared state of the verifier: the status the page reads and the cancel flag of the running replay.
/// Blocks one check downloads per node at most (a phone far behind the compiled checkpoint catches up over a few checks).
pub const MAX_BLOCKS_PER_CHECK: u64 = 2000;

#[derive(Default)]
pub struct Verifier {
    pub status: Mutex<Verification>,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    last_tip: Mutex<Option<[u8; 32]>>,
    /// Where the next check starts (`verify::sync_resume`): only the blocks after it are downloaded.
    resume: Mutex<Option<Resume>>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn short(key: &str) -> String {
    let k = key.strip_prefix("http:").or_else(|| key.strip_prefix("iroh:")).unwrap_or(key);
    let k = k.strip_prefix("https://").unwrap_or(k);
    if k.len() > 24 && !k.contains('.') { format!("{}…", &k[..12]) } else { k.to_string() }
}

impl Verifier {
    pub fn get(&self) -> Verification {
        self.status.lock().unwrap().clone()
    }

    /// Stops a replay in progress (a new tip makes it moot).
    pub fn cancel_replay(&self) {
        if let Some(c) = self.cancel.lock().unwrap().take() {
            c.store(true, Ordering::Relaxed);
        }
    }

    #[allow(dead_code)]
    pub fn last_tip(&self) -> Option<[u8; 32]> {
        *self.last_tip.lock().unwrap()
    }

    fn set_replay(&self, r: ReplayStatus) {
        self.status.lock().unwrap().replay = r;
    }

    /// The resume point of the last check (to store), or one read back from storage (used only if it fits the checkpoint).
    pub fn resume(&self) -> Option<Resume> {
        self.resume.lock().unwrap().clone()
    }
    pub fn set_resume(&self, r: Option<Resume>) {
        *self.resume.lock().unwrap() = r;
    }

    /// Header layer: asks up to five nodes, verifies the chain from the last check's resume point (or from `cp`: the first
    /// time, after a reorg below it), follows the best branch. Sets the status, bans liars (inside `verify::sync_resume`) and
    /// restricts the client to the verified branch's nodes. Returns the outcome on success.
    pub async fn check(&self, client: &Client, cp: &Checkpoint, replay_enabled: bool) -> Result<Outcome, String> {
        self.cancel_replay();
        let mut opts = CrossCheckOptions::default();
        opts.verify.max_blocks = Some(MAX_BLOCKS_PER_CHECK);
        let resume = self.resume();
        let res = sm64coin_lc::verify::sync_resume(client, cp, resume.as_ref(), &Sm64Consensus, None, &opts, &|| false).await;
        let mut st = Verification { checkpoint_height: cp.height, checked_at_ms: now_ms(), ..Default::default() };
        st.replay = if replay_enabled { ReplayStatus { state: "running", detail: "waiting".into(), ..Default::default() } } else { ReplayStatus::default() };
        match res {
            Ok(out) => {
                let on_chain = |v: &NodeVerdict| matches!(v, NodeVerdict::Agrees | NodeVerdict::Behind);
                let keys: Vec<String> = out.nodes.iter().filter(|(_, v)| on_chain(v)).map(|(k, _)| k.clone()).collect();
                st.agree = keys.len();
                st.asked = out.nodes.len();
                st.quorum = out.quorum_met;
                st.height = Some(out.best.tip.height);
                st.tip = Some(hex::encode(out.best.tip.id));
                st.level = match out.best.level {
                    Level::Full => "full",
                    Level::Structural => "structural",
                };
                st.banned = out.banned.iter().map(|k| short(k)).collect();
                st.disagree = out
                    .nodes
                    .iter()
                    .filter(|(_, v)| !on_chain(v))
                    .map(|(k, v)| {
                        (short(k), match v {
                            NodeVerdict::Forked => "served another valid chain".to_string(),
                            NodeVerdict::Liar(why) => format!("served an invalid chain: {why}"),
                            NodeVerdict::Unreachable(why) => format!("could not be checked: {why}"),
                            _ => String::new(),
                        })
                    })
                    .collect();
                let conflicted = out.nodes.iter().any(|(_, v)| matches!(v, NodeVerdict::Forked | NodeVerdict::Liar(_)));
                let nodes_word = if st.agree == 1 { "1 node agrees".to_string() } else { format!("{} nodes agree", st.agree) };
                if out.resume.is_some() {
                    *self.resume.lock().unwrap() = out.resume.clone();
                }
                if out.partial {
                    st.state = "checking";
                    st.label = format!("checking the chain (verified to block {}, catching up)", out.best.tip.height);
                } else if conflicted {
                    st.state = "conflict";
                    st.label = format!("conflict ({nodes_word}; following the heaviest valid chain)");
                } else {
                    st.state = "verified";
                    st.label = format!("verified ({nodes_word})");
                }
                *self.last_tip.lock().unwrap() = Some(out.best.tip.id);
                client.set_allowed(Some(keys));
                *self.status.lock().unwrap() = st;
                Ok(out)
            }
            Err(e) => {
                // Nothing could be checked (no node, all unreachable, or every chain invalid): reads go to any node again, and the
                // light says so.
                let why = match &e {
                    VerifyError::Cancelled => "cancelled".to_string(),
                    e => e.to_string(),
                };
                st.state = "unverified";
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                let all_banned = client.with_list(|l| !l.is_empty() && l.records().iter().all(|r| r.is_banned(now)));
                st.label = if all_banned {
                    // never a dead end: the client falls back to the banned nodes as a last resort, and says so here
                    "unverified (every node failed the chain check; reading from them anyway)".into()
                } else {
                    "unverified (no node could be checked)".into()
                };
                st.error = Some(why.clone());
                st.replay = ReplayStatus::default();
                client.set_allowed(None);
                *self.last_tip.lock().unwrap() = None;
                *self.status.lock().unwrap() = st;
                Err(why)
            }
        }
    }

    /// Replays the branch's current cycle on a blocking thread (desktop). A branch that does not replay bans its servers and
    /// returns `Err(servers)`, so the caller re-runs `check`. Cancelled by the next `check`.
    #[cfg(not(mobile))]
    pub async fn replay(self: &Arc<Self>, client: &Client, out: Outcome, rom: Option<crate::lcnet::RomFinder>) -> Result<(), Vec<String>> {
        use sm64coin_lc::replay::{ReplayBudget, ReplayProgress, Sm64Replay};
        use sm64coin_lc::verify::{Judge, JudgeVerdict};
        let cancel = Arc::new(AtomicBool::new(false));
        *self.cancel.lock().unwrap() = Some(cancel.clone());
        let servers = out.best.servers.clone();
        let me = self.clone();
        let verdict = tokio::task::spawn_blocking(move || {
            // the ROM is loaded once per process; a game already set up (or $SM64COIN_ROM) needs no bytes
            if Sm64Replay::available().is_err() {
                match rom.and_then(|f| f()) {
                    Some(bytes) => {
                        if let Err(e) = Sm64Replay::init(bytes) {
                            return JudgeVerdict::Unavailable(e);
                        }
                    }
                    None => return JudgeVerdict::Unavailable("the ROM is not available".into()),
                }
            }
            let mut r = Sm64Replay::new(ReplayBudget { extra_cycles: 0, max_blocks: None, max_duration: Some(std::time::Duration::from_secs(120)), cancel: Some(cancel) });
            let m2 = me.clone();
            r.progress = Some(Arc::new(move |p: &ReplayProgress| m2.set_replay(ReplayStatus { state: "running", done: p.done, total: p.total, detail: format!("block {}", p.height) })));
            r.judge(&out.best)
        })
        .await
        .unwrap_or_else(|e| JudgeVerdict::Unavailable(e.to_string()));
        let mut rs = ReplayStatus::default();
        let result = match verdict {
            JudgeVerdict::Valid { replayed } => {
                rs = ReplayStatus { state: "valid", done: replayed, total: replayed, detail: format!("current cycle replayed ({replayed} blocks)") };
                Ok(())
            }
            JudgeVerdict::Partial { replayed, reason } => {
                rs = ReplayStatus { state: "partial", done: replayed, total: replayed, detail: format!("{reason} after {replayed} blocks") };
                Ok(())
            }
            JudgeVerdict::Unavailable(why) => {
                rs = ReplayStatus { state: "unavailable", done: 0, total: 0, detail: why };
                Ok(())
            }
            JudgeVerdict::Invalid { height, reason } => {
                for k in &servers {
                    client.ban_liar(k);
                }
                client.set_allowed(None);
                rs.state = "invalid";
                rs.detail = format!("block {height} does not replay: {reason}");
                Err(servers)
            }
        };
        self.set_replay(rs);
        result
    }
}
