//! SM64Coin group mining in the browser: the wasm-bindgen face of `sm64coin-group` (iroh-gossip through n0's public
//! relays: a browser endpoint connects over relays only). The page's controller drives it (lib/webminer/group.js).

use sm64coin_group::{Group, Opts};
use wasm_bindgen::prelude::*;

fn err<E: std::fmt::Display>(e: E) -> JsError {
    JsError::new(&format!("{e:#}"))
}

#[wasm_bindgen]
pub struct WebGroup(Group);

fn opts(name: String, relay: Option<String>) -> Opts {
    // A browser node verifies a proof when the run is submitted: received runs are held unverified.
    Opts { name, relay: relay.filter(|r| !r.is_empty()), unverified_ok: true, platform: "web".into() }
}

#[wasm_bindgen]
impl WebGroup {
    /// Starts a group (this page is the leader).
    pub async fn create(name: String, relay: Option<String>) -> Result<WebGroup, JsError> {
        Ok(WebGroup(Group::create(opts(name, relay)).await.map_err(err)?))
    }

    /// Joins the group a team code names.
    pub async fn join(code: String, name: String, relay: Option<String>) -> Result<WebGroup, JsError> {
        Ok(WebGroup(Group::join(&code, opts(name, relay)).await.map_err(err)?))
    }

    pub fn code(&self) -> String {
        self.0.code().to_string()
    }
    pub fn is_leader(&self) -> bool {
        self.0.is_leader()
    }
    pub fn set_name(&self, n: &str) {
        self.0.set_name(n)
    }
    pub fn set_fps(&self, f: f32) {
        self.0.set_fps(f)
    }

    /// {id, leader, neighbors, members: [{id, name, fps, leader, me, age}], tgen, rgen, loggen, log: [lines since `since`]}
    pub fn state(&self, since: f64) -> String {
        let (loggen, log) = self.0.log_since(since as u64);
        serde_json::json!({
            "id": self.0.id(), "leader": self.0.is_leader(), "neighbors": self.0.neighbors(),
            "members": self.0.members(), "tgen": self.0.template_gen(), "rgen": self.0.run_gen(),
            "loggen": loggen, "log": log,
        })
        .to_string()
    }

    /// The current template as {tid, tgen, json} (None before the leader's first announcement).
    pub fn template(&self) -> Option<String> {
        self.0.template().map(|(tid, json, tgen)| serde_json::json!({"tid": format!("{tid:016x}"), "tgen": tgen, "json": json}).to_string())
    }

    /// Leader: announces the node's get_block_template JSON.
    pub fn publish_template(&self, json: &str) -> Result<(), JsError> {
        self.0.publish_template(json).map_err(err)
    }

    pub fn get_step(&self, key: &str) -> Option<String> {
        self.0.get_step(key)
    }
    /// Steps other web members shared since `since`: {sgen, steps: [{key, json}]}.
    pub fn new_steps(&self, since: f64) -> String {
        let (sgen, steps) = self.0.steps_since(since as u64);
        serde_json::json!({"sgen": sgen, "steps": steps.into_iter().map(|(key, json)| serde_json::json!({"key": key, "json": json})).collect::<Vec<_>>()}).to_string()
    }

    pub fn put_step(&self, key: &str, json: &str) {
        self.0.put_step(key, json)
    }

    /// Shares a run (its sealed proof as hex); `keys` is a JSON array of strings.
    pub fn put_run(&self, start: &str, frames: u32, proof_hex: &str, keys: &str) -> Result<(), JsError> {
        let proof = hex_decode(proof_hex).ok_or_else(|| JsError::new("bad proof hex"))?;
        let keys: Vec<String> = serde_json::from_str(keys).unwrap_or_default();
        self.0.put_run(start, frames, &proof, &keys, &[]);
        Ok(())
    }

    /// The shortest held run for the start kind as {frames, start, proof (hex), from, keys}.
    pub fn best_run(&self, start: &str) -> Option<String> {
        self.0.best_run(start).map(|r| serde_json::json!({"frames": r.frames, "start": r.start, "proof": r.proof_hex(), "from": r.from, "keys": r.keys}).to_string())
    }

    pub fn leave(&self) {
        self.0.leave()
    }
}

fn hex_decode(h: &str) -> Option<Vec<u8>> {
    if h.len() % 2 != 0 {
        return None;
    }
    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).ok()).collect()
}
