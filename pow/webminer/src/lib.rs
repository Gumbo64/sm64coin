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
}

fn err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

/// Loads the game from the ROM (once per instance).
#[wasm_bindgen]
pub fn init(rom: Vec<u8>) -> Result<(), JsError> {
    sm64pow::init(rom).map(|_| ()).map_err(err)
}

/// One campaign step (a fresh instance runs it): `child::Job` JSON in, `child::StepOut` JSON out.
#[wasm_bindgen]
pub fn run_step(job: &str) -> Result<String, JsError> {
    let job: rrt::child::Job = serde_json::from_str(job).map_err(err)?;
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
#[wasm_bindgen]
pub fn mine(request: &str) -> Result<String, JsError> {
    let r: MineRequest = serde_json::from_str(request).map_err(err)?;
    let start = if r.start == "power-on" { Start::PowerOn } else { Start::Parent };
    let params = sm64pow::params(r.retarget_difficulty.max(1));
    let seed = sm64pow::Seed::from_hex(&r.seed_hash).map_err(err)?;
    let parent = if start == Start::Parent {
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
        scratch: "/scratch".into(),
        rom: Some("/rom.z64".into()),
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
