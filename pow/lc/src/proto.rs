//! The requests and answers of the light-client network: what a client asks a node, whichever HTTP(S) node it is.
//! [`crate::exec`] maps a [`Request`] onto the node's JSON-RPC / `.bin` calls and shapes the answer into a [`Response`];
//! [`crate::client::UpstreamTransport`] runs that against a remote node over HTTP(S).

/// Largest answer a block read may build up (8 MiB).
pub const MAX_FRAME: usize = 8 << 20;

/// Largest request a client sends (1 MiB: a block blob or a transaction fits many times over).
pub const MAX_REQUEST_FRAME: usize = 1 << 20;

/// `GetHeaders.count` cap.
pub const MAX_HEADERS: u32 = 2000;

/// `GetBlocks.count` cap.
pub const MAX_BLOCKS: u32 = 100;

/// Node-list cap.
pub const MAX_NODES: u16 = 64;

/// Where a chain tip stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tip {
    pub height: u64,
    /// Block id, 32 bytes.
    pub hash: [u8; 32],
    /// The node's cumulative difficulty (the "heavier" half of the v0.4 fork choice).
    pub cumulative_difficulty: u128,
}

/// What a client needs to know about the tip without asking again (pushed as Server-Sent Events, `crate::sse`): the
/// block, and the two numbers that, with the clock, give the time limit a block found now must meet
/// (`sm64pow::rules().max_frames(difficulty, now - timestamp)`): the limit grows with the time since the parent, so a client
/// computes it locally instead of polling for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipInfo {
    /// The tip block's height (not the node's block count).
    pub height: u64,
    pub hash: [u8; 32],
    /// The tip block's timestamp (the parent of the next block).
    pub timestamp: u64,
    pub cumulative_difficulty: u128,
    /// The retarget difficulty of the next block (`get_info.difficulty`).
    pub difficulty: u64,
    /// The server's clock (unix seconds) when it sent this record (a pushed tip: at sending, not when the tip was first seen), so
    /// a client can tell how far its own clock is off.
    pub server_time: u64,
}

/// Which `.bin` (epee) call of the wallet engine to forward, body as the wallet sends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletBin {
    /// `/getblocks.bin`
    GetBlocks,
    /// `/get_outs.bin`
    GetOuts,
    /// `/gethashes.bin`
    GetHashes,
    /// `/get_transaction_pool_hashes.bin`
    GetPoolHashes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    GetTip,
    /// Block headers `from..from+count` (count <= [`MAX_HEADERS`]); the answer is the node's
    /// `get_block_headers_range` result as JSON.
    GetHeaders { from: u64, count: u32 },
    /// Whole blocks (`get_block` results as JSON) from `from`, at most `count` (<= [`MAX_BLOCKS`])
    /// and about `max_bytes`.
    GetBlocks { from: u64, count: u32, max_bytes: u32 },
    /// `get_sm64_cycle` (hash hex; empty = the tip).
    GetCycle { hash: String },
    /// `get_block_template`.
    GetTemplate { address: String, reserve: u32 },
    /// `submit_block` (the sealed block).
    SubmitBlock { blob: Vec<u8> },
    /// `submit_sm64_proof` (a runner-up block as a proof record).
    SubmitProof { blob: Vec<u8> },
    /// The wallet engine's binary calls, forwarded unchanged.
    Wallet { call: WalletBin, body: Vec<u8> },
    GetOutDistribution { amounts: Vec<u64>, from_height: u64, to_height: u64 },
    GetFee,
    /// `/send_raw_transaction`.
    SendTx { blob: Vec<u8> },
    /// Any [`crate::allowlist::RPC_ALLOWED`] JSON-RPC method of the node, `params` as JSON text. The answer is
    /// [`Response::JsonRpc`], the node's whole JSON-RPC answer (so a node-side error reaches the caller unchanged).
    JsonRpc { method: String, params: String },
    /// Any [`crate::allowlist::PATH_ALLOWED`] non-JSON-RPC call (`/get_transactions`, `/is_key_image_spent`,
    /// `/get_o_indexes.bin` ...), body as the wallet sends it; the answer is [`Response::Bin`].
    Path { path: String, body: Vec<u8> },
}

/// Why a request failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Unsupported,
    NotFound,
    /// The request did not decode / is out of range.
    Malformed,
    /// The node behind the service failed.
    Upstream(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    Tip(Tip),
    /// A node JSON-RPC `result` object, as JSON text.
    Json(String),
    /// A node `.bin` answer, unchanged.
    Bin(Vec<u8>),
    /// One JSON `get_block` result per block.
    Blocks(Vec<String>),
    Accepted,
    Rejected { reason: String },
    Error(ErrorKind),
    /// The node's whole JSON-RPC answer (`{"jsonrpc", "id", "result" | "error"}`) as JSON text; answer of `Request::JsonRpc`.
    JsonRpc(String),
}

/// A tip subscription's keepalive period (SSE `ka` events); a client that hears nothing for 2.5 times this long reconnects.
pub const KEEPALIVE_SECS: u64 = 30;

/// Tip subscriptions a server holds in all.
pub const MAX_SUBS_TOTAL: usize = 4096;

impl Request {
    /// A request a node may not serve (an older build, or a narrower allowlist): it answers `Malformed` / `Unsupported`,
    /// which is no verdict on the request, so the client moves on to another node.
    pub fn is_passthrough(&self) -> bool {
        matches!(self, Request::JsonRpc { .. } | Request::Path { .. })
    }
    /// Whether the answer may be large (bulk timeout).
    pub fn is_bulk(&self) -> bool {
        match self {
            Request::GetBlocks { .. } | Request::GetCycle { .. } | Request::Wallet { .. } => true,
            Request::JsonRpc { method, .. } => method == "get_sm64_cycle",
            Request::Path { path, .. } => path.ends_with(".bin"),
            _ => false,
        }
    }
}
