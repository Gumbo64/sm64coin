//! The SM64Coin automated miner in the browser: the native miner's RRT campaign
//! (`sm64coin_miner::rrt::mine`, the user's objective-RRT search) compiled to WebAssembly.
//!
//! The JavaScript side (web/src/lib/webminer/) provides, on `globalThis`:
//! * `sm64fs`: the files (the ROM at `/rom.z64`, the campaign's scratch), shared by every
//!   instance of the worker (obrrt_core::sys::fs);
//! * `sm64miner.run_child(job_json)`: runs [`run_step`] in a fresh instance of this module (a
//!   campaign step, as natively in a fresh `rrtrun` process) and returns its JSON;
//! * `sm64miner.log(line)`: the campaign's progress lines.
use serde::{Deserialize, Serialize};
use sm64coin_miner::rh::Attempt;
use sm64coin_miner::rrt::{self, RrtOptions};
use sm64pow::proof::Start;
use sm64pow::sm64_binds::GamePad;
use std::sync::atomic::AtomicBool;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = sm64miner, js_name = log)]
    fn js_log(line: &str);
    /// Sharing with the other workers (see rrt::Exchange): the reply, if any.
    #[wasm_bindgen(js_namespace = sm64miner, js_name = exchange)]
    fn js_exchange(kind: &str, payload: &str) -> Option<String>;

    /// One shared tree (see rrt::child::PEERS): claim the next unclaimed attempt of the round (-1: none left),
    /// publish an attempt's result bytes, wait for all of the round's, read one. The page's SharedArrayBuffer.
    #[wasm_bindgen(js_namespace = sm64miner, js_name = att_claim, catch)]
    fn js_att_claim(round: u32, n: u32) -> Result<i32, JsValue>;
    #[wasm_bindgen(js_namespace = sm64miner, js_name = att_put, catch)]
    fn js_att_put(round: u32, i: u32, bytes: &[u8]) -> Result<(), JsValue>;
    #[wasm_bindgen(js_namespace = sm64miner, js_name = att_wait, catch)]
    fn js_att_wait(round: u32, n: u32) -> Result<(), JsValue>;
    #[wasm_bindgen(js_namespace = sm64miner, js_name = att_get)]
    fn js_att_get(round: u32, i: u32) -> Vec<u8>;
}

/// The shared-tree copies' channel, through the page's JavaScript (see `rrt::child::PEERS`). WebAssembly here is
/// single-threaded, so the Send + Sync bounds hold trivially.
struct JsPeers;
fn jserr(e: JsValue) -> anyhow::Error {
    anyhow::anyhow!("{}", e.as_string().unwrap_or_else(|| format!("{e:?}")))
}
impl obrrt_search::wire::AttemptPeers for JsPeers {
    fn claim(&self, round: u64, n: usize) -> anyhow::Result<Option<usize>> {
        let i = js_att_claim(round as u32, n as u32).map_err(jserr)?;
        Ok((i >= 0).then_some(i as usize))
    }
    fn put(&self, round: u64, i: usize, bytes: &[u8]) -> anyhow::Result<()> {
        js_att_put(round as u32, i as u32, bytes).map_err(jserr)
    }
    fn collect(&self, round: u64, n: usize) -> anyhow::Result<Vec<Vec<u8>>> {
        js_att_wait(round as u32, n as u32).map_err(jserr)?;
        Ok((0..n).map(|i| js_att_get(round as u32, i as u32)).collect())
    }
}

/// Panics abort in WebAssembly (`unreachable`) and their message is lost: log it first.
fn panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| std::panic::set_hook(Box::new(|info| js_log(&format!("PANIC: {info}")))));
}

fn err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

/// Loads the game from the ROM (once per instance).
#[wasm_bindgen]
pub fn init(rom: Vec<u8>) -> Result<(), JsError> {
    panic_hook();
    sm64pow::init(rom).map(|_| ()).map_err(err)
}

/// One campaign step (a fresh instance runs it): `child::Job` JSON in, `child::StepOut` JSON out.
#[wasm_bindgen]
pub fn run_step(job: &str) -> Result<String, JsError> {
    panic_hook();
    let job: rrt::child::Job = serde_json::from_str(job).map_err(err)?;
    // The step's tree sync reaches the other workers through the page (see rrt::child::tsync).
    if job.shared {
        let _ = rrt::child::PEERS.set(std::sync::Arc::new(JsPeers));
        let _ = rrt::child::LOG.set(std::sync::Arc::new(|l: String| js_log(&l)));
    }
    let _ = rrt::child::EXCHANGE.set(std::sync::Arc::new(|kind: &str, payload: &str| js_exchange(kind, payload)));
    let out = rrt::child::run_step(&job).map_err(|e| err(format!("{e:#}")))?;
    serde_json::to_string(&out).map_err(err)
}

#[derive(Deserialize)]
pub struct MineRequest {
    /// "parent" or "power-on" (get_block_template's sm64_start; "either" is the caller's choice).
    pub start: String,
    /// The cycle's played pads up to the parent (get_sm64_cycle's played_pads), hex: replayed
    /// unperturbed from power-on they reach the parent's end state.
    #[serde(default)]
    pub parent_pads: String,
    pub seed_hash: String,
    pub retarget_difficulty: u64,
    #[serde(default = "one")]
    pub threads: usize,
    #[serde(default)]
    pub hub_frames: Option<u64>,
    #[serde(default)]
    pub course_frames: Option<u64>,
    #[serde(default)]
    pub checkpoints: Option<usize>,
    #[serde(default)]
    pub salt: u64,
    #[serde(default)]
    pub frame_cap: usize,
    /// The search's memory budget per step (MB; native default 1500, too much for several
    /// workers in one browser tab).
    #[serde(default = "mem_default")]
    pub mem_mb: usize,
    /// The page can have the search's tree sampled (`sm64miner.viz`, see rrt::child::viz).
    #[serde(default)]
    pub viz: bool,
    /// Tree sync between the workers: seconds between rounds (0: off) and states offered per round
    /// (see rrt::child::tsync). Takes effect when the page answers `exchange("tree_sync", ...)`
    /// (cross-origin isolated pages); without it the workers only share at step boundaries.
    #[serde(default = "sync_secs_default")]
    pub sync_secs: f64,
    #[serde(default = "sync_k_default")]
    pub sync_k: usize,
    /// More tree settings, "VAR=value VAR=value" (rrt::RrtOptions::extra_env). Default: rounds of 16 plans
    /// (the engine's 64 suit a many-threaded native search; with one thread a round took ~1 minute, the tree
    /// sync only runs between rounds, and BoB's step from power-on took 3-8 minutes instead of ~1).
    #[serde(default = "extra_env_default")]
    pub extra_env: String,
    /// One shared tree: `threads` copies of this same request (same salt) run as one search, each round's attempts
    /// split between them through the page's SharedArrayBuffer (sm64miner.att_*). Replaces the tree and checkpoint
    /// sync (it is one tree).
    #[serde(default)]
    pub shared_tree: bool,
}
fn extra_env_default() -> String {
    "OBRRT_ROUND=16".into()
}
// Measured (4 workers, JRB and WF course steps from a shared castle step, 9 paired salt sets):
// every 10 s / 8 states was the cheapest setting and trended faster (-34 s and -84 s mean, within
// noise); every 4 s / 12 states replayed the most and was slower on WF. Neither is significant.
fn sync_secs_default() -> f64 {
    10.0
}
fn sync_k_default() -> usize {
    8
}
fn mem_default() -> usize {
    600
}
fn one() -> usize {
    1
}

#[derive(Serialize)]
pub struct MineResult {
    /// "found", "gave" or "noroute".
    pub status: String,
    pub why: String,
    /// The sealed proof (found), hex: the template's last byte replaced by varint(len) + this.
    pub proof: String,
    pub frames: usize,
    pub sim_frames: u64,
}

/// Mines one block (the native automine's `rrt::mine` with its options), returning the sealed
/// proof when the run makes the template's progress point.
///
/// `parent_state`: the parent's end state (a save state, e.g. from the node's get_sm64_state),
/// used as is; without it the parent is rebuilt from `parent_pads`.
#[wasm_bindgen]
pub fn mine(request: &str, parent_state: Option<Vec<u8>>) -> Result<String, JsError> {
    let r: MineRequest = serde_json::from_str(request).map_err(err)?;
    let start = if r.start == "power-on" { Start::PowerOn } else { Start::Parent };
    let params = sm64pow::params(r.retarget_difficulty.max(1));
    let seed = sm64pow::Seed::from_hex(&r.seed_hash).map_err(err)?;
    let parent = if start == Start::Parent && parent_state.is_some() {
        Some(sm64pow::sm64_binds::SaveState::from_bytes(parent_state.unwrap()).map_err(err)?)
    } else if start == Start::Parent {
        let b = hex::decode(&r.parent_pads).map_err(err)?;
        let pads: Vec<GamePad> = b.chunks(4).map(|c| GamePad::new(u16::from_le_bytes([c[0], c[1]]), c[2] as i8, c[3] as i8)).collect();
        Some(sm64pow::statecodec::state_after(&pads).map_err(err)?)
    } else {
        None
    };
    let d = RrtOptions::default();
    let o = RrtOptions {
        threads: if r.shared_tree { 1 } else { r.threads.max(1) },
        hub_frames: r.hub_frames.unwrap_or(d.hub_frames),
        course_frames: r.course_frames.unwrap_or(d.course_frames),
        checkpoints: r.checkpoints.unwrap_or(d.checkpoints),
        seed: r.salt,
        frame_cap: r.frame_cap,
        mem_mb: r.mem_mb,
        viz: r.viz,
        sync_secs: if r.shared_tree { 0.0 } else { r.sync_secs },
        sync_k: r.sync_k,
        extra_env: r.extra_env.clone(),
        scratch: "/scratch".into(),
        rom: Some("/rom.z64".into()),
        // wasm is single-threaded, so the Send + Sync bounds hold trivially.
        // A shared tree needs no tree or checkpoint sync (it is one tree); the page may still hand it the castle step
        // another search made earlier (step_get).
        exchange: Some(std::sync::Arc::new(|kind: &str, payload: &str| js_exchange(kind, payload))),
        shared_tree: r.shared_tree,
        ..d
    };
    let stop = AtomicBool::new(false);
    let attempt = rrt::mine(start, parent.as_ref(), &seed, &params, &o, &stop, |m| js_log(&m)).map_err(|e| err(format!("{e:#}")))?;
    let out = match attempt {
        Attempt::Found(m) => {
            let proof = sm64pow::seal_proof_at(start, parent.as_ref(), &seed, &params, &m.pads).map_err(err)?;
            MineResult { status: "found".into(), why: String::new(), proof: hex::encode(proof), frames: m.pads.len(), sim_frames: m.sim_frames }
        }
        Attempt::Gave(why) => MineResult { status: "gave".into(), why, proof: String::new(), frames: 0, sim_frames: 0 },
        Attempt::NoRoute(why) => MineResult { status: "noroute".into(), why, proof: String::new(), frames: 0, sim_frames: 0 },
    };
    serde_json::to_string(&out).map_err(err)
}
