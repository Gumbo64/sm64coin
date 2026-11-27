//! Light-client verification: what a client checks so that no node has to be trusted
//! (`docs/LIGHT-CLIENTS.md`, "Verification: what a light client checks").
//!
//! Layers, cheapest first:
//! 1. **Anchor**: a [`Checkpoint`] compiled into the release ([`crate::checkpoints`]). A chain that
//!    does not contain it is rejected ([`VerifyError::CheckpointMismatch`]).
//! 2. **Header chain** ([`ChainState::extend`], [`verify_branch`]): from the anchor, every block's
//!    `prev_id` links, its id is recomputed from the blob ([`crate::block`]), the timestamp rules hold
//!    (after the parent, Monero's median of 60, the launch floor, not too far ahead), the retarget
//!    difficulty and the cumulative difficulty are recomputed, the run length is within the time
//!    limit, the miner's signature over the committed proof verifies, and the proof-weight
//!    (`1 + counted proofs` per block) is summed. No emulation: milliseconds per block.
//! 3. **Cross-check** ([`candidates`], [`sync`]): several nodes are asked for their tip; every distinct
//!    branch is verified from the anchor and the survivors are ordered by the consensus fork choice
//!    (`longer AND heavier`). A node that served something invalid is banned (`Client::ban_liar`).
//! 4. **Replay** (the [`Judge`] hook; [`crate::replay`] with the `replay` feature): the proof of play
//!    itself. Header checks cannot see whether a committed input hash is a real run, so only the replay
//!    turns a header-valid chain into a valid one; the winner of the fork choice is replayed (current
//!    cycle, budgeted, cancellable) before it is believed, and a branch that fails is dropped and its
//!    nodes banned.
//!
//! **Incremental checks** ([`sync_resume`], [`Resume`]): a check ends with the verifier's state a few blocks below the verified
//! tip; the next check starts there and downloads only the blocks after it. A node whose chain does not contain that point
//! (a reorg deeper than [`RESUME_MARGIN`], or a lie) makes the whole check re-anchor at the compiled checkpoint. Nodes that
//! claim the same tip are not each made to serve the whole range: above [`FULL_CHECK_BLOCKS`] one of them is verified and the
//! others are spot-checked at random heights. [`VerifyOptions::max_blocks`] caps the download of one check (the result is
//! then [`Outcome::partial`] and the next check continues from there).
//!
//! The consensus rules are not here: they are called through [`Consensus`], implemented by sm64pow
//! (`consensus` feature: [`crate::consensus::Sm64Consensus`]). Without it, [`StructuralOnly`] checks
//! linkage, ids and timestamps only (and says so: [`Level::Structural`]).
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::block::{Block, BlockError, Claims, Hash, RpcBlock};
use crate::client::{BULK_TIMEOUT, Client, READ_TIMEOUT};
use crate::exec::{MaybeSend, MaybeSync};

/// The cancel hook of the async checks (`Sync` natively so their futures stay `Send`).
#[cfg(not(target_arch = "wasm32"))]
pub type CancelFn = dyn Fn() -> bool + Sync;
#[cfg(target_arch = "wasm32")]
pub type CancelFn = dyn Fn() -> bool;
use crate::proto::{ErrorKind, Request, Response, Tip};

/// Monero's `BLOCKCHAIN_TIMESTAMP_CHECK_WINDOW` = the fork's `DIFFICULTY_WINDOW`.
pub const WINDOW: usize = 60;
/// `CRYPTONOTE_BLOCK_FUTURE_TIME_LIMIT` of the fork (seconds).
pub const FUTURE_LIMIT_SECS: u64 = 120;
/// Blocks asked per request.
pub const FETCH_BLOCKS: u32 = 50;
/// Byte budget per `GetBlocks` answer.
pub const FETCH_MAX_BYTES: u32 = 2 << 20;
/// A [`Resume`] point is kept this many blocks below the verified tip, so a reorg up to this deep is checked from it (the
/// blocks above it are fetched again on the next check); a deeper one re-anchors at the compiled checkpoint.
pub const RESUME_MARGIN: u64 = 6;
/// Up to this many blocks between the start and a tip, every node that claims the tip is verified on its own (the old
/// behaviour, cheap for incremental checks); above it one claimant per distinct tip is verified and the others are
/// spot-checked at [`SPOT_CHECKS`] random heights (and the tip).
pub const FULL_CHECK_BLOCKS: u64 = 64;
/// Random heights a node that claims an already verified tip is spot-checked at.
pub const SPOT_CHECKS: usize = 8;

// ---------------------------------------------------------------------------------------------
// Checkpoint
// ---------------------------------------------------------------------------------------------

/// The trust anchor: a block the release vouches for, with the state the rules need to continue
/// from it. Generated at release time from a trusted full node ([`Checkpoint::build`],
/// `examples/checkpoint.rs`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub height: u64,
    #[serde(with = "hex32")]
    pub id: Hash,
    /// The retarget's cumulative difficulty at `height` (what `get_block` reports).
    pub cumulative_difficulty: u128,
    /// The chain's fork-choice weight at `height`: the sum of `1 + counted proofs` over blocks 0..=height.
    pub weight: u64,
    /// The anchor block's own (retarget) difficulty: the records of the next block are judged at it.
    pub difficulty: u64,
    /// `(timestamp, cumulative difficulty)` of the last `min(60, height + 1)` blocks, ending at `height`.
    pub window: Vec<(u64, u128)>,
}

mod hexvec {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(h: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(h))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        hex::decode(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(h: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(h))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(&s).ok().and_then(|v| v.try_into().ok()).ok_or_else(|| serde::de::Error::custom("not 32 hex bytes"))
    }
}

impl Checkpoint {
    /// The checkpoint of a chain's genesis block (`id` = its hash; the genesis block is built from config, never played).
    pub fn genesis(id: Hash) -> Checkpoint {
        Checkpoint { height: 0, id, cumulative_difficulty: 1, weight: 1, difficulty: 1, window: vec![(0, 1)] }
    }

    /// Internal consistency (a corrupt table must not become a silent trust anchor).
    pub fn validate(&self) -> Result<(), String> {
        let want = ((self.height + 1) as usize).min(WINDOW);
        if self.window.len() != want {
            return Err(format!("checkpoint at {} needs a window of {want}, has {}", self.height, self.window.len()));
        }
        if self.window.last().map(|w| w.1) != Some(self.cumulative_difficulty) {
            return Err("checkpoint window does not end at its cumulative difficulty".into());
        }
        if self.weight < self.height + 1 || self.difficulty == 0 {
            return Err("checkpoint weight or difficulty is impossible".into());
        }
        Ok(())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("checkpoint serializes")
    }

    pub fn from_json(s: &str) -> Result<Checkpoint, String> {
        let c: Checkpoint = serde_json::from_str(s).map_err(|e| e.to_string())?;
        c.validate()?;
        Ok(c)
    }

    /// Runs the header rules over `blocks` (heights `0..=upto`, the first being the genesis block,
    /// trusted by its claimed hash) and returns the checkpoint at `upto`. This is what the release
    /// tool does against a trusted node.
    pub fn build(blocks: &[RpcBlock], upto: u64, rules: &dyn Consensus) -> Result<Checkpoint, VerifyError> {
        let genesis = blocks.first().ok_or_else(|| VerifyError::Source("no genesis block".into()))?;
        let mut st = ChainState::from_checkpoint(&Checkpoint::genesis(genesis.claims.hash), None)?;
        for b in blocks.iter().skip(1).take(upto as usize) {
            st.accept(b, rules, &VerifyOptions { now_secs: None, ..Default::default() })?;
        }
        if st.height != upto {
            return Err(VerifyError::Source(format!("only {} blocks, wanted up to {upto}", st.height)));
        }
        let difficulty = st.last.as_ref().map(|l| l.difficulty).unwrap_or(1);
        Ok(Checkpoint {
            height: st.height,
            id: st.id,
            cumulative_difficulty: st.cumulative,
            weight: st.weight,
            difficulty,
            window: st.window.iter().map(|w| (w.timestamp, w.cumulative)).collect(),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Consensus rules (implemented by sm64pow)
// ---------------------------------------------------------------------------------------------

/// A block's records are judged against the block before it (block h, included by block h+1).
#[derive(Clone, Copy, Debug)]
pub struct Parent1Facts<'a> {
    pub height: u64,
    pub prev_id: Hash,
    pub prev_is_genesis: bool,
    pub nonce: &'a [u8],
    pub proof: &'a [u8],
    pub difficulty: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordsFault {
    /// The witness bytes do not match what the block commits: the node serving them tampered.
    BadWitness(String),
    /// The committed records break a rule: the block is invalid.
    Invalid(String),
}

/// One side of a fork above the fork point (sm64pow's `records::Side`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Side {
    pub len: u64,
    pub weight: u64,
    pub tainted: bool,
}

/// How much of the rules a verifier applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Level {
    /// Linkage, ids, timestamps; reported difficulty and weight taken from the node.
    Structural,
    /// Everything sm64pow's rules decide without emulation: difficulty, run length limit,
    /// signatures, proof weight.
    Full,
}

/// The consensus rules the header checks call. The single implementation that matters is sm64pow's
/// ([`crate::consensus::Sm64Consensus`]); the defaults skip a rule (a `None` / `Ok(None)` answer means
/// "not checked").
pub trait Consensus: MaybeSend + MaybeSync {
    fn level(&self) -> Level {
        Level::Structural
    }
    /// No block but the genesis block may be stamped before this.
    fn launch_timestamp(&self) -> u64 {
        0
    }
    /// The next block's difficulty from the previous blocks' timestamps and cumulative difficulties (oldest first).
    fn next_difficulty(&self, _timestamps: &[u64], _cumulative: &[u128]) -> Option<u64> {
        None
    }
    /// The longest run (frames) a block at `difficulty` may have when stamped `elapsed` seconds after its parent.
    fn max_frames(&self, _difficulty: u64, _elapsed: u64) -> Option<u32> {
        None
    }
    /// Verifies the miner's signature over the committed proof; returns the miner key. `Ok(None)`: not checked.
    fn check_signature(&self, _seed: &Hash, _nonce: &[u8], _proof_id: &Hash, _frames: u32, _sig: &[u8; 64]) -> Result<Option<Hash>, String> {
        Ok(None)
    }
    /// How many of a block's included proofs count for weight (structurally; their replay is the node's
    /// and the [`Judge`]'s business).
    fn counted_records(&self, _field: &[u8], _nonce: &[u8], _parent: &Parent1Facts) -> Result<u32, RecordsFault> {
        Ok(0)
    }
    /// Fork choice: switch from `main` to `alt`?
    fn prefer_alt(&self, main: Side, alt: Side) -> bool {
        alt.len > main.len
    }
}

/// Checks only what needs no rules (see [`Level::Structural`]).
pub struct StructuralOnly;
impl Consensus for StructuralOnly {}

// ---------------------------------------------------------------------------------------------
// Errors and options
// ---------------------------------------------------------------------------------------------

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// The node was unreachable or answered unusably. Not proof of a lie.
    #[error("source failed: {0}")]
    Source(String),
    /// The node's chain does not contain the checkpoint (another chain, or a lie).
    #[error("the chain does not contain the checkpoint: {0}")]
    CheckpointMismatch(String),
    /// The node served a block that breaks a rule, or whose data contradicts the block (a lie).
    #[error("invalid block at height {height}: {reason}")]
    Invalid { height: u64, reason: String },
    #[error("cancelled")]
    Cancelled,
    /// The node's chain does not contain the [`Resume`] point (a reorg below it, or a lie): not a verdict on the node,
    /// the check re-anchors at the compiled checkpoint.
    #[error("the chain does not contain the resume point: {0}")]
    NotOnChain(String),
}

impl VerifyError {
    /// True when the node demonstrably served bad data (ban it).
    pub fn is_lie(&self) -> bool {
        matches!(self, VerifyError::CheckpointMismatch(_) | VerifyError::Invalid { .. })
    }
}

#[derive(Clone, Debug)]
pub struct VerifyOptions {
    /// Unix seconds now, for the future-timestamp rule (None: skipped).
    pub now_secs: Option<u64>,
    /// Slack on top of the daemon's 120 s limit for clock differences.
    pub future_slack_secs: u64,
    /// How many verified blocks (the newest) a [`Branch`] keeps in full for replay.
    pub keep_blocks: usize,
    /// Verify only up to this height (None: the node's tip).
    pub max_height: Option<u64>,
    /// Download at most this many blocks per node and check (None: no cap). A branch that stops below its node's tip is
    /// [`Branch::partial`]; the next check continues from its [`Resume`] point.
    pub max_blocks: Option<u64>,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self { now_secs: None, future_slack_secs: 300, keep_blocks: 512, max_height: None, max_blocks: None }
    }
}

// ---------------------------------------------------------------------------------------------
// Chain state: the header rules, one block at a time
// ---------------------------------------------------------------------------------------------

/// Everything the rules remember about one verified block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBlock {
    pub height: u64,
    pub id: Hash,
    pub prev_id: Hash,
    pub timestamp: u64,
    /// The retarget difficulty of this block.
    pub difficulty: u64,
    pub cumulative: u128,
    pub frames: u32,
    pub power_on: bool,
    /// Included proofs that count for weight (those of the *previous* block's height).
    pub counted: u32,
    /// Chain weight after this block.
    pub weight: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Win {
    height: u64,
    timestamp: u64,
    cumulative: u128,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Last {
    height: u64,
    #[serde(with = "hex32")]
    prev_id: Hash,
    #[serde(with = "hexvec")]
    nonce: Vec<u8>,
    #[serde(with = "hexvec")]
    proof: Vec<u8>,
    difficulty: u64,
}

/// The verifier's state at one block: enough to check the next.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChainState {
    pub height: u64,
    #[serde(with = "hex32")]
    pub id: Hash,
    pub timestamp: u64,
    pub cumulative: u128,
    pub weight: u64,
    window: VecDeque<Win>,
    last: Option<Last>,
}

fn median(ts: &mut Vec<u64>) -> u64 {
    // epee::misc_utils::median
    if ts.len() == 1 {
        return ts[0];
    }
    ts.sort_unstable();
    let n = ts.len() / 2;
    if ts.len() % 2 == 1 { ts[n] } else { (ts[n - 1] + ts[n]) / 2 }
}

fn invalid(height: u64, reason: impl Into<String>) -> VerifyError {
    VerifyError::Invalid { height, reason: reason.into() }
}

impl ChainState {
    /// The state at a checkpoint. `anchor` is the anchor block itself (not needed at genesis): its id must
    /// be the checkpoint's, it supplies what the records of the next block are judged against.
    pub fn from_checkpoint(cp: &Checkpoint, anchor: Option<&Block>) -> Result<ChainState, VerifyError> {
        cp.validate().map_err(VerifyError::CheckpointMismatch)?;
        let timestamp = cp.window.last().map(|w| w.0).unwrap_or(0);
        let first = cp.height + 1 - cp.window.len() as u64;
        let window = cp.window.iter().enumerate().map(|(i, w)| Win { height: first + i as u64, timestamp: w.0, cumulative: w.1 }).collect();
        let last = match (cp.height, anchor) {
            (0, _) => None,
            (_, None) => return Err(VerifyError::Source("the anchor block is needed".into())),
            (h, Some(b)) => {
                if b.id() != cp.id || b.height != h {
                    return Err(VerifyError::CheckpointMismatch(format!("block {h} is not the checkpoint's ({} vs {})", hex::encode(b.id()), hex::encode(cp.id))));
                }
                Some(Last { height: h, prev_id: b.header.prev_id, nonce: b.nonce_bytes().to_vec(), proof: b.proof.clone(), difficulty: cp.difficulty })
            }
        };
        Ok(ChainState { height: cp.height, id: cp.id, timestamp, cumulative: cp.cumulative_difficulty, weight: cp.weight, window, last })
    }

    /// Internal consistency of a state read back from storage (a corrupt file must not become a silent anchor).
    pub fn validate(&self) -> Result<(), String> {
        let w = self.window.back().ok_or("empty window")?;
        if w.height != self.height || w.timestamp != self.timestamp || w.cumulative != self.cumulative || self.window.len() > WINDOW {
            return Err("the window does not end at the state".into());
        }
        if self.window.iter().zip(self.window.iter().skip(1)).any(|(a, b)| b.height != a.height + 1) {
            return Err("the window is not consecutive".into());
        }
        if self.window.len() < WINDOW && self.window.front().is_some_and(|f| f.height != 0) {
            return Err("a short window must start at the genesis block".into());
        }
        if self.height > 0 && self.last.as_ref().is_none_or(|l| l.height != self.height) {
            return Err("the state lacks its block".into());
        }
        if self.weight < self.height + 1 {
            return Err("impossible weight".into());
        }
        Ok(())
    }

    /// The state as a block summary (the tip of a branch that verified no new block).
    fn summary(&self) -> VerifiedBlock {
        VerifiedBlock {
            height: self.height,
            id: self.id,
            prev_id: self.last.as_ref().map_or([0; 32], |l| l.prev_id),
            timestamp: self.timestamp,
            difficulty: self.last.as_ref().map_or(1, |l| l.difficulty),
            cumulative: self.cumulative,
            frames: 0,
            power_on: false,
            counted: 0,
            weight: self.weight,
        }
    }

    /// Parses and verifies the next block (`rpc` is what the node served for height `height + 1`).
    pub fn accept(&mut self, rpc: &RpcBlock, rules: &dyn Consensus, opts: &VerifyOptions) -> Result<(VerifiedBlock, Block), VerifyError> {
        let h = self.height + 1;
        let block = Block::parse(&rpc.blob).map_err(|e| invalid(h, e.to_string()))?;
        let v = self.extend(&block, &rpc.claims, rules, opts)?;
        Ok((v, block))
    }

    /// Verifies the next block against the rules and, if it passes, advances.
    pub fn extend(&mut self, b: &Block, claims: &Claims, rules: &dyn Consensus, opts: &VerifyOptions) -> Result<VerifiedBlock, VerifyError> {
        let h = self.height + 1;
        let bad = |why: &str| invalid(h, why);
        // linkage and identity
        if b.header.prev_id != self.id {
            return Err(bad("does not link to the previous block"));
        }
        if b.height != h {
            return Err(invalid(h, format!("the coinbase says height {}", b.height)));
        }
        let id = b.id();
        if claims.height != h || claims.hash != id || claims.prev_hash != b.header.prev_id || claims.timestamp != b.header.timestamp {
            return Err(bad("the node's header claims contradict the block"));
        }
        // timestamps
        let ts = b.header.timestamp;
        if ts <= self.timestamp {
            return Err(bad("timestamp is not after the parent's"));
        }
        if ts < rules.launch_timestamp() {
            return Err(invalid(h, format!("stamped {ts}, before the chain's launch at {}", rules.launch_timestamp())));
        }
        if self.window.len() >= WINDOW {
            let mut all: Vec<u64> = self.window.iter().map(|w| w.timestamp).collect();
            let m = median(&mut all);
            if ts < m {
                return Err(invalid(h, format!("timestamp {ts} is less than the median {m} of the last {WINDOW} blocks")));
            }
        }
        if let Some(now) = opts.now_secs {
            if ts > now + FUTURE_LIMIT_SECS + opts.future_slack_secs {
                return Err(bad("timestamp is too far ahead of the clock"));
            }
        }
        // difficulty (the retarget skips the genesis block, like the daemon)
        let (tss, cums): (Vec<u64>, Vec<u128>) = self.window.iter().filter(|w| w.height >= 1).map(|w| (w.timestamp, w.cumulative)).unzip();
        let difficulty = match rules.next_difficulty(&tss, &cums) {
            Some(d) => {
                if claims.difficulty != d as u128 {
                    return Err(invalid(h, format!("difficulty is {d}, the node says {}", claims.difficulty)));
                }
                d
            }
            None => u64::try_from(claims.difficulty).map_err(|_| bad("difficulty out of range"))?,
        };
        let cumulative = self.cumulative.checked_add(difficulty.max(1) as u128).ok_or_else(|| bad("cumulative difficulty overflows"))?;
        if claims.cumulative_difficulty != cumulative {
            return Err(invalid(h, format!("cumulative difficulty is {cumulative}, the node says {}", claims.cumulative_difficulty)));
        }
        // the proof of play, as far as the header shows it
        let ph = b.proof_header().ok_or_else(|| bad("the proof header does not parse"))?;
        if ph.frames == 0 {
            return Err(bad("a run of zero frames"));
        }
        if h == 1 && !ph.power_on {
            return Err(bad("the first block starts from power-on"));
        }
        if let Some(max) = rules.max_frames(difficulty, ts - self.timestamp) {
            if ph.frames > max {
                return Err(invalid(h, format!("{} frames exceeds the limit of {max}", ph.frames)));
            }
        }
        let pid = crate::block::proof_id(&b.proof);
        rules.check_signature(&b.seed(), b.nonce_bytes(), &pid, ph.frames, &b.sig).map_err(|e| invalid(h, e))?;
        // weight: this block's records are proofs for the previous block's height
        let counted = match &self.last {
            Some(l) => {
                let p = Parent1Facts { height: l.height, prev_id: l.prev_id, prev_is_genesis: l.height == 1, nonce: &l.nonce, proof: &l.proof, difficulty: l.difficulty };
                match rules.counted_records(&b.field, b.nonce_bytes(), &p) {
                    Ok(n) => n,
                    Err(RecordsFault::BadWitness(w)) => return Err(invalid(h, format!("proof records do not match what the block commits: {w}"))),
                    Err(RecordsFault::Invalid(w)) => return Err(invalid(h, format!("invalid proof records: {w}"))),
                }
            }
            None => 0,
        };
        let weight = self.weight + 1 + counted as u64;
        // commit
        self.window.push_back(Win { height: h, timestamp: ts, cumulative });
        while self.window.len() > WINDOW {
            self.window.pop_front();
        }
        self.height = h;
        self.id = id;
        self.timestamp = ts;
        self.cumulative = cumulative;
        self.weight = weight;
        self.last = Some(Last { height: h, prev_id: b.header.prev_id, nonce: b.nonce_bytes().to_vec(), proof: b.proof.clone(), difficulty });
        Ok(VerifiedBlock { height: h, id, prev_id: b.header.prev_id, timestamp: ts, difficulty, cumulative, frames: ph.frames, power_on: ph.power_on, counted, weight })
    }
}

// ---------------------------------------------------------------------------------------------
// A verified branch
// ---------------------------------------------------------------------------------------------

/// A chain from the anchor to a tip that passed the header rules.
#[derive(Clone, Debug)]
pub struct Branch {
    /// Which node verified first (`servers[0]`).
    pub source: String,
    /// Every asked node whose claimed tip is this branch's tip (an identical tip id commits to an identical chain).
    pub servers: Vec<String>,
    pub level: Level,
    /// Where this check started (the checkpoint, or the [`Resume`] point): fork choice compares branches from here.
    pub anchor_height: u64,
    pub anchor_weight: u64,
    pub tip: VerifiedBlock,
    /// Every verified block after the anchor (summaries); after a resumed check also the kept blocks below it.
    pub blocks: Vec<VerifiedBlock>,
    /// The newest verified blocks in full (up to `keep_blocks`), parallel to the end of `blocks`.
    pub raw: Vec<Block>,
    /// Stopped at [`VerifyOptions::max_blocks`] / `max_height` below the tip the node claims.
    pub partial: bool,
    /// Where the next check of this branch can start.
    pub resume: Option<Resume>,
}

impl Branch {
    pub fn side(&self) -> Side {
        Side { len: self.tip.height.saturating_sub(self.anchor_height), weight: self.tip.weight.saturating_sub(self.anchor_weight), tainted: false }
    }
    pub fn contains(&self, id: &Hash) -> bool {
        self.blocks.iter().any(|b| &b.id == id)
    }
    /// The prefix of this branch up to the block `id` (a node whose tip lies inside the branch): no download, the same
    /// chain. None when `id` is not in it.
    pub fn truncated(&self, id: &Hash, servers: Vec<String>) -> Option<Branch> {
        // a tip below this check's start (a laggard behind a resume point) is not comparable by fork choice from here
        let tip = self.blocks.iter().find(|b| &b.id == id && b.height >= self.anchor_height)?.clone();
        let off = self.blocks.len() - self.raw.len();
        let blocks: Vec<VerifiedBlock> = self.blocks.iter().filter(|b| b.height <= tip.height).cloned().collect();
        let raw: Vec<Block> = self.raw.iter().enumerate().filter(|(i, _)| self.blocks[off + i].height <= tip.height).map(|(_, b)| b.clone()).collect();
        Some(Branch { source: servers.first().cloned().unwrap_or_default(), servers, level: self.level, anchor_height: self.anchor_height, anchor_weight: self.anchor_weight, tip, blocks, raw, partial: false, resume: None })
    }
    /// The verified block at `height`, if this branch holds it.
    pub fn at(&self, height: u64) -> Option<&VerifiedBlock> {
        self.blocks.binary_search_by_key(&height, |b| b.height).ok().map(|i| &self.blocks[i])
    }
    /// Where the current cycle starts in `raw`: the last block that starts from power-on, or `extra_cycles`
    /// earlier ones (each power-on block starts a cycle). The flag says whether that index is a real
    /// cycle start; false when `raw` does not reach back that far (it then starts at 0 with a parent state
    /// nobody here knows).
    pub fn cycle_start(&self, extra_cycles: usize) -> (usize, bool) {
        let off = self.blocks.len() - self.raw.len();
        let starts: Vec<usize> = (0..self.raw.len()).filter(|&i| self.blocks[off + i].power_on).collect();
        if starts.len() > extra_cycles {
            (starts[starts.len() - 1 - extra_cycles], true)
        } else {
            (0, starts.first() == Some(&0))
        }
    }
}

/// Where a check ended: the verifier's state at a block [`RESUME_MARGIN`] below the verified tip, tied to the compiled
/// checkpoint it descends from. The next check starts here (see [`sync_resume`]). It is a cache, not an anchor of its own: a
/// chain that does not contain it re-anchors at the checkpoint, and a new release's checkpoint discards it. The kept blocks
/// (`recent`, `raw`) live in memory only (the replay of the current cycle needs them); a stored resume point has none.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resume {
    pub checkpoint_height: u64,
    #[serde(with = "hex32")]
    pub checkpoint_id: Hash,
    pub state: ChainState,
    #[serde(skip)]
    pub recent: Vec<VerifiedBlock>,
    #[serde(skip)]
    pub raw: Vec<Block>,
}

impl Resume {
    /// Usable with this compiled checkpoint (same anchor, consistent, not below it).
    pub fn fits(&self, cp: &Checkpoint) -> bool {
        self.checkpoint_id == cp.id && self.checkpoint_height == cp.height && self.state.height >= cp.height && self.state.validate().is_ok()
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("resume point serializes")
    }
    pub fn from_json(s: &str) -> Result<Resume, String> {
        let r: Resume = serde_json::from_str(s).map_err(|e| e.to_string())?;
        r.state.validate()?;
        Ok(r)
    }
}

/// Where a check starts.
#[derive(Clone, Copy, Debug)]
pub enum Start<'a> {
    Checkpoint(&'a Checkpoint),
    Resume { cp: &'a Checkpoint, resume: &'a Resume },
}

impl Start<'_> {
    pub fn height(&self) -> u64 {
        match self {
            Start::Checkpoint(c) => c.height,
            Start::Resume { resume, .. } => resume.state.height,
        }
    }
    fn checkpoint(&self) -> &Checkpoint {
        match self {
            Start::Checkpoint(c) | Start::Resume { cp: c, .. } => c,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Sources and verifying a branch
// ---------------------------------------------------------------------------------------------

/// Something that serves tips and blocks: a node through the [`Client`], or a fixture in a test.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait ChainSource: MaybeSend + MaybeSync {
    fn name(&self) -> String;
    async fn tip(&self) -> Result<Tip, String>;
    /// `count` blocks from height `from` (fewer at the end of the chain).
    async fn blocks(&self, from: u64, count: u32) -> Result<Vec<RpcBlock>, String>;
    /// The id the node claims for its block at `height` (cheaper than the block where the transport has headers).
    async fn id_at(&self, height: u64) -> Result<Hash, String> {
        self.blocks(height, 1).await?.first().map(|b| b.claims.hash).ok_or_else(|| format!("no block {height}"))
    }
}

/// One node of a [`Client`], addressed by its key.
pub struct NodeSource<'a> {
    pub client: &'a Client,
    pub key: String,
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl ChainSource for NodeSource<'_> {
    fn name(&self) -> String {
        self.key.clone()
    }
    async fn tip(&self) -> Result<Tip, String> {
        match self.client.ask_node(&self.key, &Request::GetTip, READ_TIMEOUT).await? {
            Response::Tip(t) => Ok(t),
            Response::Error(e) => Err(format!("{e:?}")),
            other => Err(format!("unexpected answer {other:?}")),
        }
    }
    async fn blocks(&self, from: u64, count: u32) -> Result<Vec<RpcBlock>, String> {
        let req = Request::GetBlocks { from, count, max_bytes: FETCH_MAX_BYTES };
        match self.client.ask_node(&self.key, &req, BULK_TIMEOUT).await? {
            Response::Blocks(list) => list.iter().map(|s| RpcBlock::from_json(s).map_err(|e: BlockError| e.to_string())).collect(),
            Response::Error(ErrorKind::NotFound) => Ok(vec![]),
            Response::Error(e) => Err(format!("{e:?}")),
            other => Err(format!("unexpected answer {other:?}")),
        }
    }
    async fn id_at(&self, height: u64) -> Result<Hash, String> {
        // one header (~600 bytes) instead of the block
        if let Ok(Response::Json(text)) = self.client.ask_node(&self.key, &Request::GetHeaders { from: height, count: 1 }, READ_TIMEOUT).await {
            let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            let h = v["headers"].get(0).ok_or("no header")?;
            if h["height"].as_u64() == Some(height) {
                if let Some(id) = h["hash"].as_str().and_then(|x| hex::decode(x).ok()).and_then(|b| <Hash>::try_from(b).ok()) {
                    return Ok(id);
                }
            }
        }
        self.blocks(height, 1).await?.first().map(|b| b.claims.hash).ok_or_else(|| format!("no block {height}"))
    }
}

/// Verifies the chain `src` serves, from `cp` to its tip (or `opts.max_height`).
pub async fn verify_branch(cp: &Checkpoint, rules: &dyn Consensus, src: &dyn ChainSource, opts: &VerifyOptions, cancel: &CancelFn) -> Result<Branch, VerifyError> {
    let tip = src.tip().await.map_err(VerifyError::Source)?;
    verify_branch_to(&Start::Checkpoint(cp), rules, src, &tip, opts, cancel).await
}

/// The verifier's state at `start` for this source (the anchor block is fetched for a checkpoint; a resume point is only
/// checked to be on the source's chain), with the kept blocks below it.
async fn open(start: &Start<'_>, src: &dyn ChainSource) -> Result<(ChainState, Vec<VerifiedBlock>, VecDeque<Block>), VerifyError> {
    match start {
        Start::Checkpoint(cp) => {
            let anchor = if cp.height == 0 {
                let first = src.blocks(0, 1).await.map_err(VerifyError::Source)?;
                match first.first() {
                    Some(g) if g.claims.hash != cp.id => return Err(VerifyError::CheckpointMismatch(format!("genesis is {}, expected {}", hex::encode(g.claims.hash), hex::encode(cp.id)))),
                    None => return Err(VerifyError::Source("no genesis block served".into())),
                    _ => None,
                }
            } else {
                let got = src.blocks(cp.height, 1).await.map_err(VerifyError::Source)?;
                let r = got.first().ok_or_else(|| VerifyError::Source("the anchor block is not served (node behind the checkpoint?)".into()))?;
                let b = Block::parse(&r.blob).map_err(|e| VerifyError::CheckpointMismatch(e.to_string()))?;
                if r.claims.hash != cp.id || b.id() != cp.id {
                    return Err(VerifyError::CheckpointMismatch(format!("block {} is {}, expected {}", cp.height, hex::encode(b.id()), hex::encode(cp.id))));
                }
                Some(b)
            };
            Ok((ChainState::from_checkpoint(cp, anchor.as_ref())?, Vec::new(), VecDeque::new()))
        }
        Start::Resume { resume, .. } => {
            let h = resume.state.height;
            let id = src.id_at(h).await.map_err(VerifyError::Source)?;
            if id != resume.state.id {
                return Err(VerifyError::NotOnChain(format!("block {h} is {}, the resume point {}", hex::encode(id), hex::encode(resume.state.id))));
            }
            Ok((resume.state.clone(), resume.recent.clone(), resume.raw.iter().cloned().collect()))
        }
    }
}

/// Verifies the chain `src` serves from `start` up to `tip` (capped by `opts.max_height` / `opts.max_blocks`).
pub async fn verify_branch_to(start: &Start<'_>, rules: &dyn Consensus, src: &dyn ChainSource, tip: &Tip, opts: &VerifyOptions, cancel: &CancelFn) -> Result<Branch, VerifyError> {
    let name = src.name();
    if tip.height < start.height() {
        return Err(VerifyError::Source(format!("tip {} is behind the start {}", tip.height, start.height())));
    }
    let (mut st, mut blocks, mut raw) = open(start, src).await?;
    let (anchor_height, anchor_weight) = (st.height, st.weight);
    let mut target = opts.max_height.map_or(tip.height, |m| m.min(tip.height));
    if let Some(n) = opts.max_blocks {
        target = target.min(st.height.saturating_add(n.max(1)));
    }
    // the margin is below the node's tip; a check capped further down resumes at its last block
    let snap_at = target.min(tip.height.saturating_sub(RESUME_MARGIN)).max(st.height);
    let mut snap = (st.height == snap_at).then(|| st.clone());
    while st.height < target {
        if cancel() {
            return Err(VerifyError::Cancelled);
        }
        let want = (target - st.height).min(FETCH_BLOCKS as u64) as u32;
        let batch = src.blocks(st.height + 1, want).await.map_err(VerifyError::Source)?;
        if batch.is_empty() {
            return Err(VerifyError::Source(format!("the node stopped serving blocks at {}", st.height)));
        }
        for r in batch.iter().take(want as usize) {
            let (v, b) = st.accept(r, rules, opts)?;
            blocks.push(v);
            raw.push_back(b);
            if raw.len() > opts.keep_blocks {
                raw.pop_front();
            }
            if st.height == snap_at {
                snap = Some(st.clone());
            }
        }
    }
    let tip_v = blocks.last().filter(|b| b.height == st.height).cloned().unwrap_or_else(|| st.summary());
    if st.height == tip.height && tip_v.id != tip.hash {
        // claims one tip, serves a chain ending elsewhere (or reorged between the two requests): not counted, not banned
        return Err(VerifyError::Source(format!("the chain served ends at {}, not at the tip it claims", hex::encode(tip_v.id))));
    }
    let raw: Vec<Block> = raw.into();
    let resume = snap.map(|state| {
        // the kept blocks up to the resume point (parallel summaries and blocks)
        let n_raw = raw.iter().zip(blocks[blocks.len() - raw.len()..].iter()).filter(|(_, v)| v.height <= state.height).count();
        let off = blocks.len() - raw.len();
        let cp = start.checkpoint();
        Resume { checkpoint_height: cp.height, checkpoint_id: cp.id, recent: blocks[off..off + n_raw].to_vec(), raw: raw[..n_raw].to_vec(), state }
    });
    Ok(Branch { servers: vec![name.clone()], source: name, level: rules.level(), anchor_height, anchor_weight, tip: tip_v, blocks, raw, partial: st.height < tip.height, resume })
}

// ---------------------------------------------------------------------------------------------
// Cross-check and fork choice
// ---------------------------------------------------------------------------------------------

/// How a node stands after the cross-check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeVerdict {
    /// Its tip is the winning branch's tip.
    Agrees,
    /// Its tip is on the winning branch, below the tip.
    Behind,
    /// A valid chain that lost the fork choice.
    Forked,
    /// Could not be asked / answered unusably.
    Unreachable(String),
    /// Served something invalid: banned.
    Liar(String),
}

#[derive(Clone, Debug)]
pub struct CrossCheckOptions {
    /// How many nodes to ask.
    pub nodes: usize,
    /// How many must agree on the winning tip for [`Outcome::quorum_met`].
    pub quorum: usize,
    pub verify: VerifyOptions,
    /// See [`FULL_CHECK_BLOCKS`].
    pub full_check_blocks: u64,
}

impl Default for CrossCheckOptions {
    fn default() -> Self {
        Self { nodes: 5, quorum: 3, verify: VerifyOptions::default(), full_check_blocks: FULL_CHECK_BLOCKS }
    }
}

/// Header-valid branches, best fork choice first, and the nodes that failed.
#[derive(Debug)]
pub struct Candidates {
    pub branches: Vec<Branch>,
    /// Unreachable nodes and liars (already banned).
    pub failed: Vec<(String, NodeVerdict)>,
    /// A node's chain does not contain the resume point: the caller re-anchors at the checkpoint.
    pub not_on_chain: bool,
}

/// Sorts `branches` best-first by the fork choice (repeatedly picks the branch no other beats).
pub fn order_by_fork_choice(rules: &dyn Consensus, mut branches: Vec<Branch>) -> Vec<Branch> {
    let mut out = Vec::with_capacity(branches.len());
    while !branches.is_empty() {
        let mut best = 0;
        for i in 1..branches.len() {
            if rules.prefer_alt(branches[best].side(), branches[i].side()) {
                best = i;
            }
        }
        out.push(branches.remove(best));
    }
    out
}

/// Asks nodes for tips, verifies every distinct branch from the checkpoint and orders the valid ones by
/// fork choice. Nodes whose chain is invalid are banned. No replay here (see [`sync`]).
pub async fn candidates(client: &Client, cp: &Checkpoint, rules: &dyn Consensus, opts: &CrossCheckOptions, cancel: &CancelFn) -> Result<Candidates, VerifyError> {
    candidates_from(client, &Start::Checkpoint(cp), rules, opts, cancel).await
}

fn merge(branches: &mut Vec<Branch>, key: String, b: Branch) {
    match branches.iter_mut().find(|x| x.tip.id == b.tip.id) {
        // an identical tip id commits to an identical chain
        Some(x) => x.servers.push(key),
        None => branches.push(b),
    }
}

/// Spot-checks that `src` serves `branch`'s blocks: the tip and [`SPOT_CHECKS`] random heights above the start. A block that
/// is not the verified one is a lie (the node claims this tip, and a tip id commits to every block below it).
async fn spot_check(client: &Client, branch: &Branch, start: u64, src: &dyn ChainSource) -> Result<(), VerifyError> {
    let span = branch.tip.height - start;
    let mut heights = vec![branch.tip.height];
    for _ in 0..SPOT_CHECKS.min(span as usize) {
        heights.push(start + 1 + client.rand() % span);
    }
    heights.sort_unstable();
    heights.dedup();
    for h in heights {
        let got = src.blocks(h, 1).await.map_err(VerifyError::Source)?;
        let r = got.first().ok_or_else(|| VerifyError::Source(format!("block {h} is not served")))?;
        let want = branch.at(h).ok_or_else(|| VerifyError::Source(format!("block {h} is not in the branch")))?;
        let b = Block::parse(&r.blob).map_err(|e| invalid(h, e.to_string()))?;
        if b.id() != want.id || r.claims.hash != want.id || r.claims.cumulative_difficulty != want.cumulative {
            return Err(invalid(h, "the block served is not the one on the chain this node claims"));
        }
    }
    Ok(())
}

/// [`candidates`] from any [`Start`]. Tips are asked first; nodes that claim the same tip form a group. A group whose tip
/// lies inside an already verified branch gets that branch cut at its tip (no download). Otherwise, within [`FULL_CHECK_BLOCKS`] of the start every node of
/// the group is verified on its own; above it one is verified and the others are spot-checked ([`spot_check`]).
pub async fn candidates_from(client: &Client, start: &Start<'_>, rules: &dyn Consensus, opts: &CrossCheckOptions, cancel: &CancelFn) -> Result<Candidates, VerifyError> {
    let keys = client.pick_keys(opts.nodes);
    if keys.is_empty() {
        return Err(VerifyError::Source("no usable node".into()));
    }
    let sources: Vec<NodeSource> = keys.iter().map(|k| NodeSource { client, key: k.clone() }).collect();
    let tips = futures_util::future::join_all(sources.iter().map(|s| async move { s.tip().await })).await;
    let mut failed: Vec<(String, NodeVerdict)> = Vec::new();
    let mut groups: Vec<(Tip, Vec<usize>)> = Vec::new();
    for (i, t) in tips.into_iter().enumerate() {
        match t {
            Ok(t) => match groups.iter_mut().find(|(g, _)| g.hash == t.hash && g.height == t.height) {
                Some((_, v)) => v.push(i),
                None => groups.push((t, vec![i])),
            },
            Err(e) => failed.push((keys[i].clone(), NodeVerdict::Unreachable(e))),
        }
    }
    groups.sort_by(|a, b| b.0.height.cmp(&a.0.height));
    let mut branches: Vec<Branch> = Vec::new();
    let mut not_on_chain = false;
    // what one node's verification said
    let mut settle = |key: &str, r: Result<Branch, VerifyError>, branches: &mut Vec<Branch>, failed: &mut Vec<(String, NodeVerdict)>| -> Result<bool, VerifyError> {
        match r {
            Ok(b) => {
                merge(branches, key.to_string(), b);
                Ok(true)
            }
            Err(VerifyError::Cancelled) => Err(VerifyError::Cancelled),
            Err(e @ VerifyError::NotOnChain(_)) => {
                not_on_chain = true;
                failed.push((key.to_string(), NodeVerdict::Unreachable(e.to_string())));
                Ok(false)
            }
            Err(e) if e.is_lie() => {
                client.ban_liar(key);
                failed.push((key.to_string(), NodeVerdict::Liar(e.to_string())));
                Ok(false)
            }
            Err(e) => {
                failed.push((key.to_string(), NodeVerdict::Unreachable(e.to_string())));
                Ok(false)
            }
        }
    };
    for (tip, members) in groups {
        if cancel() {
            return Err(VerifyError::Cancelled);
        }
        // a tip inside a verified branch: the same chain, cut there (no download)
        if let Some(t) = branches.iter().find(|b| b.tip.id != tip.hash).and_then(|b| b.truncated(&tip.hash, members.iter().map(|&i| keys[i].clone()).collect())) {
            branches.push(t);
            continue;
        }
        if tip.height.saturating_sub(start.height()) <= opts.full_check_blocks {
            let results = futures_util::future::join_all(members.iter().map(|&i| {
                let (s, tip) = (&sources[i], &tip);
                async move { verify_branch_to(start, rules, s, tip, &opts.verify, cancel).await }
            }))
            .await;
            for (&i, r) in members.iter().zip(results) {
                settle(&keys[i], r, &mut branches, &mut failed)?;
            }
            continue;
        }
        // one verified, the rest spot-checked against it
        let mut verified: Option<Hash> = None;
        let mut rest = Vec::new();
        for &i in &members {
            if verified.is_some() {
                rest.push(i);
                continue;
            }
            let r = verify_branch_to(start, rules, &sources[i], &tip, &opts.verify, cancel).await;
            let id = r.as_ref().ok().map(|b| b.tip.id);
            if settle(&keys[i], r, &mut branches, &mut failed)? {
                verified = id;
            }
        }
        let Some(id) = verified else { continue };
        let b = branches.iter().find(|b| b.tip.id == id).expect("just merged").clone();
        for i in rest {
            match spot_check(client, &b, start.height(), &sources[i]).await {
                Ok(()) => branches.iter_mut().find(|x| x.tip.id == id).expect("present").servers.push(keys[i].clone()),
                Err(e) => {
                    settle(&keys[i], Err(e), &mut branches, &mut failed)?;
                }
            }
        }
    }
    if branches.is_empty() {
        return Err(VerifyError::Source(format!("no node served a valid chain: {}", failed.iter().map(|(k, v)| format!("{k}: {v:?}")).collect::<Vec<_>>().join("; "))));
    }
    Ok(Candidates { branches: order_by_fork_choice(rules, branches), failed, not_on_chain })
}

/// What the proof of play checks (the expensive layer) said about a branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JudgeVerdict {
    /// Every replayed block's proof is valid and reaches its progress point.
    Valid { replayed: usize },
    /// A block's run does not replay.
    Invalid { height: u64, reason: String },
    /// Stopped (cancel flag, budget): nothing wrong found in what was replayed.
    Partial { replayed: usize, reason: String },
    /// Replay is not possible here (no ROM, feature off): nothing was checked.
    Unavailable(String),
}

/// The replay hook. [`crate::replay::Sm64Replay`] (feature `replay`) implements it with sm64pow. It is
/// synchronous and CPU-heavy: call [`sync`] from a worker thread (or `spawn_blocking`) when that matters.
pub trait Judge: MaybeSend + MaybeSync {
    fn judge(&self, branch: &Branch) -> JudgeVerdict;
}

#[derive(Debug)]
pub struct Outcome {
    /// The branch to follow (header-valid and, if a judge ran, replayed as far as budgeted).
    pub best: Branch,
    pub replay: Option<JudgeVerdict>,
    pub nodes: Vec<(String, NodeVerdict)>,
    /// At least `quorum` nodes agree on the best tip.
    pub quorum_met: bool,
    /// Nodes banned during this sync (header liars and replay liars).
    pub banned: Vec<String>,
    /// The best branch stopped at the per-check cap below its nodes' tip: verified so far, the next check continues.
    pub partial: bool,
    /// Started from a resume point, but a node's chain did not contain it: this check ran from the checkpoint.
    pub reanchored: bool,
    /// Where the next check starts ([`sync_resume`]).
    pub resume: Option<Resume>,
}

/// Cross-check, then replay the winner (if a judge is given). A branch whose replay fails is dropped and
/// every node that served it is banned (`Client::ban_liar`), then the next best is judged.
pub async fn sync(client: &Client, cp: &Checkpoint, rules: &dyn Consensus, judge: Option<&dyn Judge>, opts: &CrossCheckOptions, cancel: &CancelFn) -> Result<Outcome, VerifyError> {
    sync_resume(client, cp, None, rules, judge, opts, cancel).await
}

/// [`sync`], starting from `resume` (the previous outcome's [`Outcome::resume`]) when it fits `cp`: only the blocks after
/// it are downloaded. When a node's chain does not contain it (a reorg below it), the check runs again from `cp`.
pub async fn sync_resume(client: &Client, cp: &Checkpoint, resume: Option<&Resume>, rules: &dyn Consensus, judge: Option<&dyn Judge>, opts: &CrossCheckOptions, cancel: &CancelFn) -> Result<Outcome, VerifyError> {
    if let Some(r) = resume.filter(|r| r.fits(cp)) {
        match candidates_from(client, &Start::Resume { cp, resume: r }, rules, opts, cancel).await {
            Ok(c) if !c.not_on_chain => return finish(client, cp, c, judge, opts, false),
            Err(VerifyError::Cancelled) => return Err(VerifyError::Cancelled),
            // a reorg below the resume point, or nothing verifiable from it: from the checkpoint
            Ok(_) | Err(_) => {}
        }
        let c = candidates(client, cp, rules, opts, cancel).await?;
        return finish(client, cp, c, judge, opts, true);
    }
    let c = candidates(client, cp, rules, opts, cancel).await?;
    finish(client, cp, c, judge, opts, false)
}

fn finish(client: &Client, cp: &Checkpoint, c: Candidates, judge: Option<&dyn Judge>, opts: &CrossCheckOptions, reanchored: bool) -> Result<Outcome, VerifyError> {
    let mut banned: Vec<String> = c.failed.iter().filter(|(_, v)| matches!(v, NodeVerdict::Liar(_))).map(|(k, _)| k.clone()).collect();
    let mut nodes = c.failed.clone();
    let mut chosen: Option<(usize, Option<JudgeVerdict>)> = None;
    let mut dropped = vec![false; c.branches.len()];
    for (i, b) in c.branches.iter().enumerate() {
        let verdict = judge.map(|j| j.judge(b));
        if let Some(JudgeVerdict::Invalid { height, reason }) = &verdict {
            for key in &b.servers {
                client.ban_liar(key);
                banned.push(key.clone());
                nodes.push((key.clone(), NodeVerdict::Liar(format!("block {height} does not replay: {reason}"))));
            }
            dropped[i] = true;
            continue;
        }
        chosen = Some((i, verdict));
        break;
    }
    let Some((i, replay)) = chosen else {
        return Err(VerifyError::Invalid { height: cp.height + 1, reason: "every candidate chain failed its replay".into() });
    };
    let best = &c.branches[i];
    for (j, b) in c.branches.iter().enumerate() {
        if dropped[j] {
            continue;
        }
        let v = if j == i {
            NodeVerdict::Agrees
        } else if best.contains(&b.tip.id) {
            NodeVerdict::Behind
        } else {
            NodeVerdict::Forked
        };
        // servers of the winning tip agree; servers of a shorter tip on the winning chain are behind
        for key in &b.servers {
            nodes.push((key.clone(), v.clone()));
        }
    }
    let agreeing = nodes.iter().filter(|(_, v)| *v == NodeVerdict::Agrees).count();
    let best = c.branches.into_iter().nth(i).unwrap();
    let (partial, resume) = (best.partial, best.resume.clone());
    Ok(Outcome { best, replay, nodes, quorum_met: agreeing >= opts.quorum, banned, partial, reanchored, resume })
}

// ---------------------------------------------------------------------------------------------
// History between two checkpoints (the random old-cycle spot check)
// ---------------------------------------------------------------------------------------------

/// Fetches the blocks strictly between `lo` and up to `hi` (heights `lo.height + 1 ..= hi.height`) and checks
/// that they form the chain between the two trusted checkpoints: every `prev_id` links, the first links to
/// `lo`, the last is `hi`. No rules are applied (the chain between two checkpoints is what both vouch for);
/// the blocks are authentic, which is what a replay of old history needs. Pick `lo` and `hi` from
/// [`crate::checkpoints`] a few blocks apart to bound the download.
pub async fn backfill(lo: &Checkpoint, hi: &Checkpoint, src: &dyn ChainSource, cancel: &CancelFn) -> Result<Vec<Block>, VerifyError> {
    if hi.height <= lo.height {
        return Err(VerifyError::Source("backfill needs lo below hi".into()));
    }
    let mut blocks: Vec<Block> = Vec::new();
    let mut next = lo.height + 1;
    while next <= hi.height {
        if cancel() {
            return Err(VerifyError::Cancelled);
        }
        let want = (hi.height + 1 - next).min(FETCH_BLOCKS as u64) as u32;
        let batch = src.blocks(next, want).await.map_err(VerifyError::Source)?;
        if batch.is_empty() {
            return Err(VerifyError::Source(format!("the node stopped serving blocks at {next}")));
        }
        for r in batch.iter().take(want as usize) {
            let b = Block::parse(&r.blob).map_err(|e| invalid(next, e.to_string()))?;
            if b.height != next || r.claims.hash != b.id() {
                return Err(invalid(next, "the served block is not the one asked for"));
            }
            blocks.push(b);
            next += 1;
        }
    }
    let mut want_id = hi.id;
    for b in blocks.iter().rev() {
        if b.id() != want_id {
            return Err(VerifyError::CheckpointMismatch(format!("block {} does not lead to the checkpoint at {}", b.height, hi.height)));
        }
        want_id = b.header.prev_id;
    }
    if want_id != lo.id {
        return Err(VerifyError::CheckpointMismatch(format!("the blocks do not start at the checkpoint at {}", lo.height)));
    }
    Ok(blocks)
}
