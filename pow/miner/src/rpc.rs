//! Minimal JSON-RPC client for the SM64Coin daemon.

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};

pub struct Daemon {
    url: String,
    agent: ureq::Agent,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Template {
    pub blocktemplate_blob: String,
    pub blockhashing_blob: String,
    pub difficulty: u64,
    pub height: u64,
    pub prev_hash: String,
    pub expected_reward: u64,
    pub sm64_seed: u32,
    /// The full perturbation seed (cn_fast_hash of the hashing blob), hex.
    pub sm64_seed_hash: String,
    pub sm64_reseed_frames: u32,
    pub sm64_timestamp: u64,
    pub sm64_max_frames: u32,
    pub sm64_random_amount: u32,
    pub sm64_window_length: u32,
    pub sm64_burst_length: u32,
    pub sm64_retarget_difficulty: u64,
    pub sm64_easier_at: u64,
    pub sm64_rules: String,
    pub sm64_goal: String,
}

impl Daemon {
    pub fn new(url: &str) -> Daemon {
        let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(60)).build();
        Daemon { url: url.trim_end_matches('/').to_string(), agent }
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 0, "method": method, "params": params});
        let resp: Value = self.agent.post(&format!("{}/json_rpc", self.url)).send_json(body)?.into_json()?;
        if let Some(e) = resp.get("error") {
            bail!("{method}: {}", e.get("message").and_then(|m| m.as_str()).unwrap_or("error"));
        }
        resp.get("result").cloned().ok_or_else(|| anyhow!("{method}: no result"))
    }

    pub fn template(&self, address: &str) -> Result<Template> {
        let r = self.call("get_block_template", json!({"wallet_address": address, "reserve_size": 0}))?;
        Ok(serde_json::from_value(r)?)
    }

    pub fn top_hash(&self) -> Result<String> {
        let r = self.call("get_info", json!({}))?;
        Ok(r["top_block_hash"].as_str().unwrap_or_default().to_string())
    }

    /// Chain height (number of blocks, genesis included).
    pub fn height(&self) -> Result<u64> {
        let r = self.call("get_info", json!({}))?;
        r["height"].as_u64().ok_or_else(|| anyhow!("get_info: no height"))
    }

    /// The winning run that sealed block `height`: its hash and pads.
    pub fn block_run(&self, height: u64) -> Result<(String, Vec<sm64_binds::GamePad>)> {
        let r = self.call("get_block", json!({"height": height}))?;
        let hash = r["block_header"]["hash"].as_str().unwrap_or_default().to_string();
        let raw = hex::decode(r["sm64_pads"].as_str().unwrap_or_default())?;
        let pads = sm64pow::proof::decode(&[&[0u8][..], &raw].concat(), usize::MAX)?;
        Ok((hash, pads))
    }

    pub fn submit(&self, block_hex: &str) -> Result<()> {
        self.call("submit_block", json!([block_hex]))?;
        Ok(())
    }
}

/// The template with `proof` in place of its empty proof (the blob's last
/// field, so its final byte is the empty proof's length, 0).
pub fn seal(template_hex: &str, proof: &[u8]) -> Result<String> {
    let mut blob = hex::decode(template_hex)?;
    if blob.pop() != Some(0) {
        bail!("template does not end with an empty proof; is this an SM64Coin daemon?");
    }
    let mut n = proof.len() as u64;
    while n >= 0x80 {
        blob.push((n as u8) | 0x80);
        n >>= 7;
    }
    blob.push(n as u8);
    blob.extend_from_slice(proof);
    Ok(hex::encode(blob))
}
