//! Typed decoding of SM64Coin blocks for the light client: parse a block blob, recompute its id
//! (Monero's block hashing with the proof-of-play folded in), read what the verifier needs
//! (timestamp, prev id, height, miner nonce, committed proof header, signature, `sm64_proofs` field).
//!
//! No dependencies besides Keccak: builds for wasm32 and Android. This is a minimal port of the
//! parsing in `sm64pow/src/{template,commit}.rs` and of the fork's `get_block_id_blob`; with the
//! `replay` feature a test checks it against sm64pow on the live-chain fixtures. The rules
//! themselves are NOT here: see [`crate::verify::Consensus`].
//!
//! Block blob layout: `major minor timestamp prev_id[32] nonce[4]` (varints for the first three),
//! the v2 coinbase, `varint(n) || n tx hashes`, `varint(len) || proof`, `sig[64]`, then an optional
//! `varint(len) || sm64_proofs` (v0.4 included proofs).
//! Block id = `keccak(varint(len(b)) || b)` with
//! `b = hashing_blob || proof_id[32] || sig[64]`, `hashing_blob = header || tree_hash(txs) || varint(count)`.
use serde_json::Value;
use sha3::{Digest, Keccak256};

pub type Hash = [u8; 32];

pub const MAX_HEADER: usize = 128;
pub const MAX_MINER_TX: usize = 16384;
pub const MAX_TX_HASHES: usize = 1024;
/// `proof.rs::MAX_FRAMES`.
pub const MAX_PROOF_FRAMES: u64 = 1 << 20;
const BAD_TAG: &[u8; 8] = b"sm64bad!";

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum BlockError {
    #[error("malformed block: {0}")]
    Malformed(&'static str),
    #[error("bad json answer: {0}")]
    Json(String),
}
const E: BlockError = BlockError::Malformed("truncated or non-canonical");

pub fn keccak(parts: &[&[u8]]) -> Hash {
    let mut k = Keccak256::new();
    for p in parts {
        k.update(p);
    }
    k.finalize().into()
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn eof(&self) -> bool {
        self.p >= self.b.len()
    }
    fn u8(&mut self) -> Result<u8, BlockError> {
        let v = *self.b.get(self.p).ok_or(E)?;
        self.p += 1;
        Ok(v)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], BlockError> {
        let end = self.p.checked_add(n).filter(|&e| e <= self.b.len()).ok_or(E)?;
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn h32(&mut self) -> Result<Hash, BlockError> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    /// Canonical LEB128, at most 63 bits like Monero's reader.
    fn varint(&mut self) -> Result<u64, BlockError> {
        let mut v = 0u64;
        for i in 0..10 {
            let c = self.u8()?;
            let low = (c & 0x7f) as u64;
            if i == 9 && low > 1 {
                return Err(E);
            }
            v |= low << (7 * i);
            if c & 0x80 == 0 {
                if c == 0 && i > 0 {
                    return Err(E);
                }
                return Ok(v);
            }
        }
        Err(E)
    }
}

pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Monero's `tree_hash` (src/crypto/tree-hash.c).
pub fn tree_hash(h: &[Hash]) -> Hash {
    let pair = |a: &Hash, b: &Hash| keccak(&[a, b]);
    match h.len() {
        0 => [0; 32],
        1 => h[0],
        2 => pair(&h[0], &h[1]),
        count => {
            let mut cnt = 2;
            while cnt < count {
                cnt <<= 1;
            }
            cnt >>= 1;
            let k = 2 * cnt - count;
            let mut ints = vec![[0u8; 32]; cnt];
            ints[..k].copy_from_slice(&h[..k]);
            let mut i = k;
            for j in k..cnt {
                ints[j] = pair(&h[i], &h[i + 1]);
                i += 2;
            }
            while cnt > 2 {
                cnt >>= 1;
                for j in 0..cnt {
                    ints[j] = pair(&ints[2 * j], &ints[2 * j + 1]);
                }
            }
            pair(&ints[0], &ints[1])
        }
    }
}

/// The 4 header fields the rules read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub major: u64,
    pub minor: u64,
    pub timestamp: u64,
    pub prev_id: Hash,
    pub nonce: u32,
}

/// The committed proof header (`commit.rs`): `varint(frames) || start || input_hash || encoding`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProofHeader {
    pub frames: u32,
    /// True: the run starts from power-on (a new cycle); false: from the parent's end state.
    pub power_on: bool,
    pub input_hash: Hash,
}

impl ProofHeader {
    /// None when the header does not parse (the block's proof id is then `keccak("sm64bad!" || proof)`).
    pub fn parse(proof: &[u8]) -> Option<ProofHeader> {
        let mut r = Rd { b: proof, p: 0 };
        let frames = r.varint().ok()?;
        if frames > MAX_PROOF_FRAMES {
            return None;
        }
        let power_on = match r.u8().ok()? {
            0 => false,
            1 => true,
            _ => return None,
        };
        Some(ProofHeader { frames: frames as u32, power_on, input_hash: r.h32().ok()? })
    }
}

/// What a block id commits to for its proof (committed inputs, v0.4+).
pub fn proof_id(proof: &[u8]) -> Hash {
    match ProofHeader::parse(proof) {
        Some(h) => h.input_hash,
        None => keccak(&[BAD_TAG, proof]),
    }
}

/// A parsed block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub header: Header,
    /// The header bytes as they appear in the blob.
    pub header_bytes: Vec<u8>,
    pub miner_tx: Vec<u8>,
    pub tx_hashes: Vec<Hash>,
    pub proof: Vec<u8>,
    pub sig: [u8; 64],
    /// The optional `sm64_proofs` string (empty: absent or empty).
    pub field: Vec<u8>,
    /// Height from the coinbase's `txin_gen`.
    pub height: u64,
    /// The coinbase's first extra nonce (carries the miner key and the v0.4 TLVs).
    pub nonce: Option<Vec<u8>>,
    /// Hash of the coinbase transaction.
    pub coinbase_hash: Hash,
}

/// A v2 coinbase at the start of `t`: (total length, prefix length, height, extra).
fn parse_miner_tx(t: &[u8]) -> Result<(usize, usize, u64, &[u8]), BlockError> {
    let mut r = Rd { b: t, p: 0 };
    if r.varint()? != 2 {
        return Err(BlockError::Malformed("coinbase is not v2"));
    }
    r.varint()?; // unlock_time
    if r.varint()? != 1 || r.u8()? != 0xff {
        return Err(BlockError::Malformed("coinbase input is not txin_gen"));
    }
    let height = r.varint()?;
    let nout = r.varint()?;
    for _ in 0..nout {
        r.varint()?;
        match r.u8()? {
            0x02 => drop(r.take(32)?),
            0x03 => drop(r.take(33)?),
            _ => return Err(BlockError::Malformed("coinbase output type")),
        }
    }
    let n = r.varint()? as usize;
    let extra = r.take(n)?;
    let prefix = r.p;
    if r.u8()? != 0 {
        return Err(BlockError::Malformed("coinbase rct type"));
    }
    Ok((r.p, prefix, height, extra))
}

/// The first extra nonce that parses (fields before a parse error still count): `parse_tx_extra`.
fn first_extra_nonce(extra: &[u8]) -> Option<Vec<u8>> {
    let mut r = Rd { b: extra, p: 0 };
    while !r.eof() {
        let f = (|| -> Result<Option<&[u8]>, BlockError> {
            match r.u8()? {
                0x00 => {
                    let mut k = 0;
                    while !r.eof() {
                        if r.u8()? != 0 {
                            return Err(E);
                        }
                        k += 1;
                        if k > 254 {
                            return Err(E);
                        }
                    }
                }
                0x01 => drop(r.take(32)?),
                0x02 => {
                    let n = r.varint()? as usize;
                    if n > 255 {
                        return Err(E);
                    }
                    return Ok(Some(r.take(n)?));
                }
                0x03 => {
                    let n = r.varint()? as usize;
                    let mut m = Rd { b: r.take(n)?, p: 0 };
                    m.varint()?;
                    m.h32()?;
                }
                0x04 => {
                    let n = r.varint()? as usize;
                    r.take(n.checked_mul(32).ok_or(E)?)?;
                }
                0xde => {
                    let n = r.varint()? as usize;
                    r.take(n)?;
                }
                _ => return Err(E),
            }
            Ok(None)
        })();
        match f {
            Ok(Some(n)) => return Some(n.to_vec()),
            Ok(None) => {}
            Err(_) => return None,
        }
    }
    None
}

impl Block {
    /// Parses a full block blob (what `get_block` returns as `blob`).
    pub fn parse(blob: &[u8]) -> Result<Block, BlockError> {
        let mut r = Rd { b: blob, p: 0 };
        let major = r.varint()?;
        let minor = r.varint()?;
        let timestamp = r.varint()?;
        let prev_id = r.h32()?;
        let nonce = u32::from_le_bytes(r.take(4)?.try_into().unwrap());
        let header_bytes = blob[..r.p].to_vec();
        let start = r.p;
        let (len, prefix, height, extra) = parse_miner_tx(&blob[start..])?;
        let miner_tx = blob[start..start + len].to_vec();
        let coinbase_hash = keccak(&[&keccak(&[&miner_tx[..prefix]]), &keccak(&[&[0]]), &[0; 32]]);
        let nonce_bytes = first_extra_nonce(extra);
        r.p = start + len;
        let n = r.varint()? as usize;
        if n > MAX_TX_HASHES {
            return Err(BlockError::Malformed("too many transactions"));
        }
        let tx_hashes = (0..n).map(|_| r.h32()).collect::<Result<Vec<_>, _>>()?;
        let plen = r.varint()? as usize;
        let proof = r.take(plen)?.to_vec();
        let sig: [u8; 64] = r.take(64)?.try_into().unwrap();
        let field = if r.eof() {
            vec![]
        } else {
            let n = r.varint()? as usize;
            r.take(n)?.to_vec()
        };
        if !r.eof() || header_bytes.len() > MAX_HEADER || miner_tx.len() > MAX_MINER_TX {
            return Err(BlockError::Malformed("trailing bytes or oversized part"));
        }
        Ok(Block {
            header: Header { major, minor, timestamp, prev_id, nonce },
            header_bytes,
            miner_tx,
            tx_hashes,
            proof,
            sig,
            field,
            height,
            nonce: nonce_bytes,
            coinbase_hash,
        })
    }

    /// `get_block_hashing_blob` with the timestamp `ts` (the block's own, or 0 for the seed).
    fn hashing_blob(&self, ts: u64) -> Vec<u8> {
        let mut r = Rd { b: &self.header_bytes, p: 0 };
        r.varint().unwrap();
        r.varint().unwrap();
        let ts_at = r.p;
        r.varint().unwrap();
        let mut hb = self.header_bytes[..ts_at].to_vec();
        put_varint(&mut hb, ts);
        hb.extend_from_slice(&self.header_bytes[r.p..]);
        let mut leaves = vec![self.coinbase_hash];
        leaves.extend_from_slice(&self.tx_hashes);
        hb.extend_from_slice(&tree_hash(&leaves));
        put_varint(&mut hb, leaves.len() as u64);
        hb
    }

    /// The block id: what the node reports as `hash`.
    pub fn id(&self) -> Hash {
        let mut b = self.hashing_blob(self.header.timestamp);
        b.extend_from_slice(&proof_id(&self.proof));
        b.extend_from_slice(&self.sig);
        let mut len = Vec::new();
        put_varint(&mut len, b.len() as u64);
        keccak(&[&len, &b])
    }

    /// The seed the run and its signature are bound to: the hashing blob with timestamp 0.
    pub fn seed(&self) -> Hash {
        keccak(&[&self.hashing_blob(0)])
    }

    /// The committed proof header (None: does not parse, so the block cannot be valid).
    pub fn proof_header(&self) -> Option<ProofHeader> {
        ProofHeader::parse(&self.proof)
    }

    /// The miner's coinbase extra nonce, or empty.
    pub fn nonce_bytes(&self) -> &[u8] {
        self.nonce.as_deref().unwrap_or(&[])
    }
}

/// The `get_block` / `get_block_header_by_height` fields a node *claims*; never trusted, only
/// cross-checked against what the blob says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claims {
    pub height: u64,
    pub hash: Hash,
    pub prev_hash: Hash,
    pub timestamp: u64,
    pub difficulty: u128,
    pub cumulative_difficulty: u128,
}

/// One `get_block` answer: the blob and the node's claims about it.
#[derive(Clone, Debug)]
pub struct RpcBlock {
    pub blob: Vec<u8>,
    pub claims: Claims,
}

fn hex32(v: &Value) -> Option<Hash> {
    hex::decode(v.as_str()?).ok()?.try_into().ok()
}

fn wide(h: &Value, wide_key: &str, narrow: &str) -> Option<u128> {
    match h.get(wide_key).and_then(|v| v.as_str()) {
        Some(s) => u128::from_str_radix(s.trim_start_matches("0x"), 16).ok(),
        None => h.get(narrow)?.as_u64().map(|x| x as u128),
    }
}

impl Claims {
    /// From a `block_header` object (`get_block`, `get_block_header_by_height`, headers range).
    pub fn from_header(h: &Value) -> Result<Claims, BlockError> {
        let bad = |what: &str| BlockError::Json(format!("block_header has no valid {what}"));
        Ok(Claims {
            height: h.get("height").and_then(|v| v.as_u64()).ok_or_else(|| bad("height"))?,
            hash: h.get("hash").and_then(hex32).ok_or_else(|| bad("hash"))?,
            prev_hash: h.get("prev_hash").and_then(hex32).ok_or_else(|| bad("prev_hash"))?,
            timestamp: h.get("timestamp").and_then(|v| v.as_u64()).ok_or_else(|| bad("timestamp"))?,
            difficulty: wide(h, "wide_difficulty", "difficulty").ok_or_else(|| bad("difficulty"))?,
            cumulative_difficulty: wide(h, "wide_cumulative_difficulty", "cumulative_difficulty").ok_or_else(|| bad("cumulative_difficulty"))?,
        })
    }
}

impl RpcBlock {
    /// From one entry of `Response::Blocks` (the `result` of `get_block`).
    pub fn from_json(s: &str) -> Result<RpcBlock, BlockError> {
        let v: Value = serde_json::from_str(s).map_err(|e| BlockError::Json(e.to_string()))?;
        let blob = v.get("blob").and_then(|b| b.as_str()).and_then(|b| hex::decode(b).ok()).ok_or_else(|| BlockError::Json("no blob".into()))?;
        let claims = Claims::from_header(v.get("block_header").ok_or_else(|| BlockError::Json("no block_header".into()))?)?;
        Ok(RpcBlock { blob, claims })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn fixture() -> Vec<RpcBlock> {
        include_str!("../tests/fixtures/mainnet_blocks.jsonl").lines().map(|l| RpcBlock::from_json(l).unwrap()).collect()
    }

    #[test]
    fn ids_of_the_live_chain() {
        let blocks = fixture();
        assert!(blocks.len() >= 17);
        for b in &blocks[1..] {
            let p = Block::parse(&b.blob).unwrap();
            assert_eq!(p.id(), b.claims.hash, "height {}", b.claims.height);
            assert_eq!(p.height, b.claims.height);
            assert_eq!(p.header.prev_id, b.claims.prev_hash);
            assert_eq!(p.header.timestamp, b.claims.timestamp);
        }
    }

    #[test]
    fn garbage_does_not_parse() {
        assert!(Block::parse(&[]).is_err());
        assert!(Block::parse(&[1, 2, 3]).is_err());
        let b = &fixture()[3];
        for cut in [1, 40, b.blob.len() / 2, b.blob.len() - 5] {
            assert!(Block::parse(&b.blob[..cut]).is_err(), "cut at {cut}");
        }
        let mut t = b.blob.clone();
        t.push(0);
        assert!(Block::parse(&t).is_err());
    }
}
