//! The wire protocol of the light-client network: messages, framing and size limits.
//!
//! One QUIC bi-stream per request. Each direction is a sequence of frames
//! `u32 big-endian length || postcard(message)`; a request is one frame, an answer is one frame
//! (the doc's streamed multi-frame answers are not implemented: the server sends one frame per
//! request, `GetBlocks` returns a bounded batch instead).
//!
//! Versioning: the major version is in the ALPN ([`ALPN`]); `Hello.minor` and `Hello.features` evolve
//! without a new ALPN. Messages are only ever *appended* to the enums within a major version (postcard
//! encodes the variant index), so old peers answer new variants with `Error::Unsupported`/close.
use serde::{Deserialize, Serialize};

/// ALPN of the service. A breaking change is `sm64coin/light/2`, served side by side for a release.
pub const ALPN: &[u8] = b"sm64coin/light/1";
/// This implementation's minor version.
pub const MINOR: u16 = 0;

/// Largest frame an answer may have (8 MiB).
pub const MAX_FRAME: usize = 8 << 20;
/// Largest frame a request may have (1 MiB: a block blob or a transaction fits many times over).
pub const MAX_REQUEST_FRAME: usize = 1 << 20;
/// `GetHeaders.count` cap.
pub const MAX_HEADERS: u32 = 2000;
/// `GetBlocks.count` cap.
pub const MAX_BLOCKS: u32 = 100;
/// `GetNodes.max` and node-list cap.
pub const MAX_NODES: u16 = 64;
/// Concurrent streams per connection the server accepts.
pub const MAX_STREAMS_PER_CONN: u32 = 8;
/// A signed [`NodeAnnouncement`] expires after this many seconds (24 h).
pub const ANNOUNCEMENT_TTL_SECS: u64 = 24 * 3600;

/// Feature bits of `Hello.features`.
pub mod features {
    pub const CYCLE: u64 = 1 << 0;
    pub const PROOF_RECORDS: u64 = 1 << 1;
    pub const TREE_SYNC: u64 = 1 << 2;
    pub const WALLET: u64 = 1 << 3;
    pub const PROBE: u64 = 1 << 4;
    /// Features this implementation's server offers.
    pub const SERVED: u64 = CYCLE | PROOF_RECORDS | WALLET | PROBE;
}

/// Flag bits of [`NodeAnnouncement::flags`].
pub mod node_flags {
    /// Runs an iroh relay open to light clients.
    pub const RELAY: u32 = 1 << 0;
    /// Serves this protocol.
    pub const LC: u32 = 1 << 1;
    /// Keeps the whole chain (not pruned).
    pub const ARCHIVE: u32 = 1 << 2;
}

/// A node's own signed statement of how to reach it. Verified with [`NodeAnnouncement::verify`]
/// (feature `iroh-client`); a forwarding node cannot alter it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAnnouncement {
    /// ed25519 public key of the iroh endpoint.
    pub endpoint_id: [u8; 32],
    pub relay_url: Option<String>,
    /// A plain `https://host[:port]` JSON-RPC front, if the node has one.
    pub https_rpc: Option<String>,
    /// Direct addresses ("ip:port") known at signing time.
    pub direct_addrs: Vec<String>,
    pub height: u64,
    pub flags: u32,
    pub version: u32,
    /// Unix seconds after which the announcement is stale.
    pub expires: u64,
    /// ed25519 signature over [`NodeAnnouncement::signing_bytes`].
    pub signature: Vec<u8>,
}

impl NodeAnnouncement {
    /// The bytes that are signed: a domain tag and the postcard encoding of everything but the signature.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.signature = Vec::new();
        let mut out = b"sm64coin/light/announcement/1".to_vec();
        out.extend(postcard::to_stdvec(&unsigned).expect("announcement encodes"));
        out
    }
    pub fn is_expired(&self, now: u64) -> bool {
        now >= self.expires
    }
}

/// Where a chain tip stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tip {
    pub height: u64,
    /// Block id, 32 bytes.
    pub hash: [u8; 32],
    /// The node's cumulative difficulty (the "heavier" half of the v0.4 fork choice).
    pub cumulative_difficulty: u128,
}

/// Which `.bin` (epee) call of the wallet engine to forward, body as the wallet sends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Must be the first request on a connection. `network` = genesis block hash.
    Hello { minor: u16, features: u64, network: [u8; 32] },
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
    /// Light-client servers the node knows (signed announcements).
    GetNodes { max: u16 },
    /// "Connect back to the IP you see me at, on this TCP port": the relay reachability self-test.
    Probe { tcp_port: u16 },
}

/// Why a request failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    /// Over budget: try again after `retry_ms`.
    Busy { retry_ms: u32 },
    TooLarge,
    Unsupported,
    NotFound,
    /// The request did not decode / is out of range.
    Malformed,
    /// No (valid) `Hello` yet, or wrong network.
    BadHello,
    /// The node behind the service failed.
    Upstream(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Hello { minor: u16, features: u64, network: [u8; 32], height: u64, tip: [u8; 32], relay_url: Option<String> },
    Tip(Tip),
    /// A node JSON-RPC `result` object, as JSON text.
    Json(String),
    /// A node `.bin` answer, unchanged.
    Bin(Vec<u8>),
    /// One JSON `get_block` result per block.
    Blocks(Vec<String>),
    Accepted,
    Rejected { reason: String },
    Nodes(Vec<NodeAnnouncement>),
    /// `observed`: the IP the server saw the prober at (None: only a relayed path);
    /// `reachable`: the TCP connect back to `observed:tcp_port` worked.
    Probe { observed: Option<String>, reachable: bool },
    Error(ErrorKind),
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame of {0} bytes exceeds the limit of {1}")]
    TooLarge(usize, usize),
    #[error("truncated frame")]
    Truncated,
    #[error("malformed message: {0}")]
    Malformed(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// `len || postcard(msg)`.
pub fn encode_frame<T: Serialize>(msg: &T, max: usize) -> Result<Vec<u8>, FrameError> {
    let body = postcard::to_stdvec(msg).map_err(|e| FrameError::Malformed(e.to_string()))?;
    if body.len() > max {
        return Err(FrameError::TooLarge(body.len(), max));
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend((body.len() as u32).to_be_bytes());
    out.extend(body);
    Ok(out)
}

/// Decodes one complete frame (`len || body`, nothing after it).
pub fn decode_frame<T: for<'a> Deserialize<'a>>(frame: &[u8], max: usize) -> Result<T, FrameError> {
    if frame.len() < 4 {
        return Err(FrameError::Truncated);
    }
    let len = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
    if len > max {
        return Err(FrameError::TooLarge(len, max));
    }
    if frame.len() != 4 + len {
        return Err(FrameError::Truncated);
    }
    decode_body(&frame[4..])
}

pub fn decode_body<T: for<'a> Deserialize<'a>>(body: &[u8]) -> Result<T, FrameError> {
    postcard::from_bytes(body).map_err(|e| FrameError::Malformed(e.to_string()))
}

/// Reads one frame body. `Ok(None)`: clean end of stream before a frame. The length is checked
/// against `max` *before* anything is allocated.
#[cfg(feature = "tokio-io")]
pub async fn read_frame<R: tokio::io::AsyncRead + Unpin>(r: &mut R, max: usize) -> Result<Option<Vec<u8>>, FrameError> {
    use tokio::io::AsyncReadExt;
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        let n = r.read(&mut len[got..]).await?;
        if n == 0 {
            return if got == 0 { Ok(None) } else { Err(FrameError::Truncated) };
        }
        got += n;
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(FrameError::TooLarge(len, max));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await.map_err(|e| if e.kind() == std::io::ErrorKind::UnexpectedEof { FrameError::Truncated } else { e.into() })?;
    Ok(Some(body))
}

#[cfg(feature = "tokio-io")]
pub async fn write_frame<W: tokio::io::AsyncWrite + Unpin, T: Serialize>(w: &mut W, msg: &T, max: usize) -> Result<(), FrameError> {
    use tokio::io::AsyncWriteExt;
    let frame = encode_frame(msg, max)?;
    w.write_all(&frame).await?;
    Ok(())
}

impl Request {
    /// A short name, for logs and rate-limit classes.
    pub fn name(&self) -> &'static str {
        match self {
            Request::Hello { .. } => "hello",
            Request::GetTip => "get_tip",
            Request::GetHeaders { .. } => "get_headers",
            Request::GetBlocks { .. } => "get_blocks",
            Request::GetCycle { .. } => "get_cycle",
            Request::GetTemplate { .. } => "get_template",
            Request::SubmitBlock { .. } => "submit_block",
            Request::SubmitProof { .. } => "submit_proof",
            Request::Wallet { .. } => "wallet",
            Request::GetOutDistribution { .. } => "get_out_distribution",
            Request::GetFee => "get_fee",
            Request::SendTx { .. } => "send_tx",
            Request::GetNodes { .. } => "get_nodes",
            Request::Probe { .. } => "probe",
        }
    }
    /// Budget cost in tokens (a plain read costs 1; heavy ones more, DOS.md style).
    pub fn cost(&self) -> u32 {
        match self {
            Request::Hello { .. } | Request::GetTip | Request::GetFee | Request::GetNodes { .. } => 1,
            Request::GetTemplate { .. } | Request::GetOutDistribution { .. } | Request::SendTx { .. } => 2,
            Request::SubmitBlock { .. } | Request::SubmitProof { .. } | Request::Probe { .. } => 4,
            Request::GetHeaders { .. } | Request::Wallet { .. } => 5,
            Request::GetBlocks { .. } | Request::GetCycle { .. } => 10,
        }
    }
}

impl NodeAnnouncement {
    /// Checks the signature (made by the endpoint's own key) and the expiry.
    pub fn verify(&self, now: u64) -> Result<(), &'static str> {
        if self.is_expired(now) {
            return Err("expired");
        }
        if self.expires > now + 2 * ANNOUNCEMENT_TTL_SECS {
            return Err("expiry too far ahead");
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&self.endpoint_id).map_err(|_| "bad key")?;
        let sig: [u8; 64] = self.signature.as_slice().try_into().map_err(|_| "bad signature length")?;
        key.verify_strict(&self.signing_bytes(), &ed25519_dalek::Signature::from_bytes(&sig)).map_err(|_| "bad signature")
    }
}
