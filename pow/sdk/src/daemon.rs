//! Minimal JSON-RPC client for the SM64Coin daemon.

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use sm64_binds::GamePad;
use sm64pow::sign::Keypair;

pub struct Daemon {
    url: String,
    agent: ureq::Agent,
}

/// The fields of `get_block_template` a bot cares about.
#[derive(Debug, Clone, Deserialize)]
pub struct Template {
    pub blocktemplate_blob: String,
    pub difficulty: u64,
    pub height: u64,
    pub prev_hash: String,
    pub expected_reward: u64,
    /// The perturbation seed: `cn_fast_hash(hashing blob)`, 64 hex characters.
    pub sm64_seed_hash: String,
    pub sm64_reseed_frames: u32,
    pub sm64_max_frames: u32,
    pub sm64_random_amount: u32,
    #[serde(default)]
    pub sm64_window_length: u32,
    #[serde(default)]
    pub sm64_burst_length: u32,
    /// Unix time after which the task gets easier (0: never).
    #[serde(default)]
    pub sm64_easier_at: u64,
    #[serde(default)]
    pub sm64_rules: String,
    #[serde(default)]
    pub sm64_goal: String,
    /// The retarget difficulty (the time limit at timestamp-elapsed 0 follows from it).
    #[serde(default)]
    pub sm64_retarget_difficulty: u64,
    /// The template's timestamp.
    #[serde(default)]
    pub sm64_timestamp: u64,
    /// Where this block's run may start: "parent", "power-on" or "either".
    #[serde(default)]
    pub sm64_start: String,
    /// Unix time from which a power-on start is allowed (the stall escape).
    #[serde(default)]
    pub sm64_power_on_at: u64,
    #[serde(default)]
    pub sm64_parent_timestamp: u64,
    #[serde(default)]
    pub sm64_parent_stars: u32,
    #[serde(default)]
    pub sm64_parent_keys: u32,
    #[serde(default)]
    pub sm64_cycle_start_height: u64,
    /// Only on a `sm64_frames` request: when a run of that many frames becomes valid.
    #[serde(default)]
    pub sm64_valid_at: u64,
    /// The chain's tip when the template was made (set by [`Daemon::template`]).
    /// Usually `prev_hash`; not when the node builds around an equivocated tip, so
    /// "a new block landed" means the tip moved from THIS, not from `prev_hash`.
    #[serde(default)]
    pub tip: String,
}

/// `get_sm64_cycle`: the current cycle's blocks and every frame played in it.
#[derive(Debug, Clone)]
pub struct Cycle {
    pub blocks: Vec<CycleBlock>,
    /// The pad played on every frame, from the cycle's first block to the tip.
    pub pads: Vec<GamePad>,
    pub stars: u32,
    pub keys: u32,
    pub grand_star: bool,
    pub ends_cycle: bool,
}

#[derive(Debug, Clone)]
pub struct CycleBlock {
    pub height: u64,
    pub hash: String,
    pub frames: u32,
    pub power_on: bool,
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

    /// A block template paying `address`, for the miner key `key` (v0.3: the
    /// daemon puts `key.extra_nonce()` into the coinbase, so the seed covers
    /// the key; use the same key for every template call of one run).
    pub fn template(&self, address: &str, key: &Keypair) -> Result<Template> {
        let tip = self.top_hash().unwrap_or_default();
        let r = self.call("get_block_template", json!({"wallet_address": address, "extra_nonce": hex::encode(key.extra_nonce())}))?;
        let mut t: Template = serde_json::from_value(r)?;
        t.tip = if tip.is_empty() { t.prev_hash.clone() } else { tip };
        Ok(t)
    }

    /// A template that also says when a run of `frames` frames becomes valid
    /// (`sm64_valid_at`). Pass the key of the run's template: only that field
    /// is meant to be used (a fresh template may carry new mempool transactions,
    /// i.e. another seed).
    pub fn template_valid_at(&self, address: &str, key: &Keypair, frames: u32) -> Result<Template> {
        let r = self.call("get_block_template", json!({"wallet_address": address, "extra_nonce": hex::encode(key.extra_nonce()), "sm64_frames": frames}))?;
        Ok(serde_json::from_value(r)?)
    }

    /// The cycle ending at `tip` (None: the chain's top): its blocks and the
    /// pads played on every frame. Replaying the pads unperturbed from
    /// power-on reaches the tip's end state.
    pub fn cycle(&self, tip: Option<&str>) -> Result<Cycle> {
        let r = match tip {
            Some(h) => self.call("get_sm64_cycle", json!({"hash": h}))?,
            None => self.call("get_sm64_cycle", json!({}))?,
        };
        let raw = hex::decode(r["played_pads"].as_str().unwrap_or_default())?;
        let pads = raw.chunks_exact(4).map(sm64pow::proof::pad_from).collect();
        let blocks = r["blocks"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|b| CycleBlock {
                        height: b["height"].as_u64().unwrap_or(0),
                        hash: b["hash"].as_str().unwrap_or_default().to_string(),
                        frames: b["frames"].as_u64().unwrap_or(0) as u32,
                        power_on: b["power_on"].as_bool().unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Cycle {
            blocks,
            pads,
            stars: r["stars"].as_u64().unwrap_or(0) as u32,
            keys: r["keys"].as_u64().unwrap_or(0) as u32,
            grand_star: r["grand_star"].as_bool().unwrap_or(false),
            ends_cycle: r["ends_cycle"].as_bool().unwrap_or(false),
        })
    }

    /// Hash of the chain's tip: it changes when a new block arrives.
    pub fn top_hash(&self) -> Result<String> {
        let r = self.call("get_info", json!({}))?;
        Ok(r["top_block_hash"].as_str().unwrap_or_default().to_string())
    }

    /// Submits a sealed block (hex).
    pub fn submit(&self, block_hex: &str) -> Result<()> {
        self.call("submit_block", json!([block_hex]))?;
        Ok(())
    }
}

impl Template {
    /// The perturbation seed (`sm64_seed_hash`) as bytes.
    pub fn seed_bytes(&self) -> Result<[u8; 32]> {
        hex::decode(&self.sm64_seed_hash)?.try_into().map_err(|_| anyhow!("sm64_seed_hash is not 32 bytes"))
    }
}

/// Signs `proof` (a sealed proof blob) for `template` with the key whose
/// `extra_nonce()` the template was requested with. The result is the block's
/// `sm64_sig`; pass it to [`seal`].
pub fn sign_proof(key: &Keypair, template: &Template, proof: &[u8]) -> Result<[u8; 64]> {
    sm64pow::mining::sign_proof(key, &template.seed_bytes()?, proof)
}

/// The submit-ready block (hex): the template with `proof` and its signature
/// `sig` ([`sign_proof`]) in place of the empty ones, and with the header's
/// timestamp replaced by `timestamp` when given (the seed doesn't include it,
/// so a miner stamps its block "now" right before submitting).
pub fn seal(template_hex: &str, proof: &[u8], sig: &[u8; 64], timestamp: Option<u64>) -> Result<String> {
    Ok(hex::encode(sm64pow::mining::seal_block(&hex::decode(template_hex)?, proof, sig, timestamp)?))
}

fn put_varint(out: &mut Vec<u8>, mut n: u64) {
    while n >= 0x80 {
        out.push((n as u8) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

/// Length of the varint at the start of `b`.
fn varint_len(b: &[u8]) -> Result<usize> {
    b.iter().position(|x| x & 0x80 == 0).map(|i| i + 1).ok_or_else(|| anyhow!("truncated varint"))
}

/// The block blob with its header timestamp replaced (header: varint major
/// version, varint minor version, varint timestamp, prev_id, nonce, ...).
pub fn set_timestamp(blob: &[u8], ts: u64) -> Result<Vec<u8>> {
    let a = varint_len(blob)?;
    let b = a + varint_len(&blob[a..])?;
    let c = b + varint_len(&blob[b..])?;
    let mut out = blob[..b].to_vec();
    put_varint(&mut out, ts);
    out.extend_from_slice(&blob[c..]);
    Ok(out)
}

/// The header timestamp of a block blob.
pub fn get_timestamp(blob: &[u8]) -> Result<u64> {
    let a = varint_len(blob)?;
    let b = a + varint_len(&blob[a..])?;
    let c = b + varint_len(&blob[b..])?;
    let mut n = 0u64;
    for (i, x) in blob[b..c].iter().enumerate() {
        n |= ((x & 0x7f) as u64) << (7 * i);
    }
    Ok(n)
}

/// The game at the end of `cycle` (the next block's start state when it
/// continues from its parent) and its progress: the cycle's played pads
/// replayed unperturbed from power-on.
pub fn cycle_end_state(cycle: &Cycle) -> Result<(sm64_binds::SaveState, sm64pow::game::Progress)> {
    let mut g = sm64pow::generator()?.create_game()?;
    for p in &cycle.pads {
        g.step_game(*p)?;
    }
    let progress = sm64pow::game::read_progress(&g)?;
    if progress.stars != cycle.stars || progress.keys != cycle.keys || progress.grand_star != cycle.grand_star {
        bail!("replaying the cycle gives {progress:?}, the daemon says {} stars {} keys", cycle.stars, cycle.keys);
    }
    Ok((g.save_state(), progress))
}
