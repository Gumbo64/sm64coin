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
    /// Anytime mining (the best run so far is known, see rrt::RrtOptions::beat): its length (0: none), its
    /// objective keys as a goal bias, a prefix of it to go on from (hex pads), the whole run and its step starts
    /// for the trees to branch off (hex pads).
    #[serde(default)]
    pub beat: usize,
    #[serde(default)]
    pub guide_keys: Vec<String>,
    #[serde(default = "guide_weight_default")]
    pub guide_weight: f64,
    #[serde(default)]
    pub prefix_pads: String,
    #[serde(default)]
    pub seed_pads: String,
    #[serde(default)]
    pub seed_starts: Vec<usize>,
    #[serde(default = "seed_chunk_default")]
    pub seed_chunk: usize,
    #[serde(default = "seed_weight_default")]
    pub seed_weight: f64,
}
fn guide_weight_default() -> f64 {
    20.0
}
fn seed_chunk_default() -> usize {
    45
}
fn seed_weight_default() -> f64 {
    2.0
}
/// Pads as hex: per frame u16 button (little endian), stick x, stick y (the cycle's played_pads format).
fn pads_from_hex(h: &str) -> Result<Vec<GamePad>, JsError> {
    let b = hex::decode(h).map_err(err)?;
    Ok(b.chunks_exact(4).map(|c| GamePad::new(u16::from_le_bytes([c[0], c[1]]), c[2] as i8, c[3] as i8)).collect())
}
fn pads_to_hex(p: &[GamePad]) -> String {
    let mut b = Vec::with_capacity(p.len() * 4);
    for g in p {
        b.extend_from_slice(&g.button.to_le_bytes());
        b.push(g.stick_x as u8);
        b.push(g.stick_y as u8);
    }
    hex::encode(b)
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
    /// The run (found): its pads (hex, see `pads_to_hex`), the keys of the objectives that made its path and the played length at the start of each step: what the page shares with the other workers.
    pub pads: String,
    pub keys: Vec<String>,
    pub starts: Vec<usize>,
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
        threads: r.threads.max(1),
        hub_frames: r.hub_frames.unwrap_or(d.hub_frames),
        course_frames: r.course_frames.unwrap_or(d.course_frames),
        checkpoints: r.checkpoints.unwrap_or(d.checkpoints),
        seed: r.salt,
        frame_cap: r.frame_cap,
        mem_mb: r.mem_mb,
        viz: r.viz,
        beat: r.beat,
        guide_keys: r.guide_keys.clone(),
        guide_weight: r.guide_weight,
        prefix: pads_from_hex(&r.prefix_pads)?,
        seed_run: (!r.seed_pads.is_empty()).then(|| pads_from_hex(&r.seed_pads).map(|pads| rrt::SeedRun { pads, starts: r.seed_starts.clone() })).transpose()?,
        seed_chunk: r.seed_chunk,
        seed_weight: r.seed_weight,
        sync_secs: r.sync_secs,
        sync_k: r.sync_k,
        extra_env: r.extra_env.clone(),
        scratch: "/scratch".into(),
        rom: Some("/rom.z64".into()),
        // wasm is single-threaded, so the Send + Sync bounds hold trivially.
        exchange: Some(std::sync::Arc::new(|kind: &str, payload: &str| js_exchange(kind, payload))),
        ..d
    };
    let stop = AtomicBool::new(false);
    let attempt = rrt::mine(start, parent.as_ref(), &seed, &params, &o, &stop, |m| js_log(&m)).map_err(|e| err(format!("{e:#}")))?;
    let out = match attempt {
        Attempt::Found(m) => {
            let proof = sm64pow::seal_proof_at(start, parent.as_ref(), &seed, &params, &m.pads).map_err(err)?;
            MineResult { status: "found".into(), why: String::new(), proof: hex::encode(proof), frames: m.pads.len(), sim_frames: m.sim_frames, pads: pads_to_hex(&m.pads), keys: m.keys.clone(), starts: m.starts.clone() }
        }
        Attempt::Gave(why) => MineResult { status: "gave".into(), why, proof: String::new(), frames: 0, sim_frames: 0, pads: String::new(), keys: Vec::new(), starts: Vec::new() },
        Attempt::NoRoute(why) => MineResult { status: "noroute".into(), why, proof: String::new(), frames: 0, sim_frames: 0, pads: String::new(), keys: Vec::new(), starts: Vec::new() },
    };
    serde_json::to_string(&out).map_err(err)
}
