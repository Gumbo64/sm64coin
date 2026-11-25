//! The proof-of-play blob (layer 1): what a block carries instead of a nonce.
//!
//! To the daemon it is opaque bytes (`block::sm64_proof`, the last field of
//! the block blob, at most 256 KiB); only this crate knows its format:
//! `varint(frames) || start || codec id || codec body`. `start` says where
//! the run begins ([`Start`]). The body holds only the player's inputs on
//! frames the perturbation didn't poke, in order: poked frames get their
//! random pad from the perturbation (regenerated from the seed when
//! verifying), so they are never recorded.
//!
//! The stateless codecs live here: 0 raw (4 bytes per pad), 1 run-length,
//! 2 xz. The block id commits to the decoded proof ([`crate::proof_id()`]), so
//! the daemon may re-encode a stateless proof to the smallest of them. The
//! state-aware codecs (3 statectx1, 4/5 statemix) are in
//! [`crate::statecodec`]; they are decoded during the replay.
//!
//! Guarantees: every decoder is canonical (one encoding per input sequence:
//! canonical varints, no zero or repeated runs) and refuses to expand past
//! its frame limit (zip-bomb guard, [`MAX_FRAMES`]).
//!
//! To add a codec: implement [`Codec`], give it an unused id, and add it to
//! [`CODECS`]. Old blocks stay valid because their codec id still decodes.
//! Adding a codec is a hard fork for nodes that don't know it (they reject
//! blocks using it); removing one breaks old blocks, so don't.
//!
//! Read next: pow/perturb/src/lib.rs (the pokes), pow/perturb/src/sm64.rs,
//! then emulator.rs and game.rs.

use anyhow::{Result, anyhow, bail};
use sm64_binds::GamePad;

/// Hard cap on a decoded proof, whatever the rules: a decoder must refuse to
/// expand past this (zip-bomb guard).
pub const MAX_FRAMES: usize = 1 << 20;

pub trait Codec: Sync {
    /// The leading byte of blobs in this codec.
    fn id(&self) -> u8;
    fn name(&self) -> &'static str;
    fn encode(&self, pads: &[GamePad]) -> Vec<u8>;
    /// Must reject malformed input and anything over `max_frames` pads.
    fn decode(&self, body: &[u8], max_frames: usize) -> Result<Vec<GamePad>>;
}

/// Every codec a node accepts.
#[cfg(not(target_arch = "wasm32"))]
pub static CODECS: &[&dyn Codec] = &[&Raw, &RunLength, &Xz];
/// The browser build (the web miner) has no liblzma: no xz codec. It still seals with the
/// smallest of the others (the state codecs usually win), and nodes re-encode stateless proofs.
#[cfg(target_arch = "wasm32")]
pub static CODECS: &[&dyn Codec] = &[&Raw, &RunLength];

pub fn codec(id: u8) -> Option<&'static dyn Codec> {
    CODECS.iter().copied().find(|c| c.id() == id)
}

pub fn encode_with(c: &dyn Codec, pads: &[GamePad]) -> Vec<u8> {
    let mut out = vec![c.id()];
    out.extend(c.encode(pads));
    out
}

/// The smallest encoding among all codecs.
pub fn encode_best(pads: &[GamePad]) -> Vec<u8> {
    CODECS.iter().map(|c| encode_with(*c, pads)).min_by_key(|b| b.len()).unwrap()
}

pub fn decode(blob: &[u8], max_frames: usize) -> Result<Vec<GamePad>> {
    let (&id, body) = blob.split_first().ok_or_else(|| anyhow!("empty proof"))?;
    let c = codec(id).ok_or_else(|| anyhow!("unknown proof codec {id}"))?;
    c.decode(body, max_frames.min(MAX_FRAMES))
}

/// Where a block's run begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Start {
    /// From the parent block's end state (the normal case).
    #[default]
    Parent = 0,
    /// From power-on: the first block of a cycle (after the genesis block or
    /// a cycle-ending block), or a restart (see `Rules::restart_delay`).
    PowerOn = 1,
}

/// A run as a block carries it: how many frames it lasted, where it started,
/// and the inputs of the frames that weren't poked.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Proof {
    pub frames: u32,
    pub start: Start,
    pub inputs: Vec<GamePad>,
}

pub fn encode_proof_with(c: &dyn Codec, proof: &Proof) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint(&mut out, proof.frames as u64);
    out.push(proof.start as u8);
    out.extend(encode_with(c, &proof.inputs));
    out
}

/// The smallest encoding among all codecs.
pub fn encode_proof(proof: &Proof) -> Vec<u8> {
    CODECS.iter().map(|c| encode_proof_with(*c, proof)).min_by_key(|b| b.len()).unwrap()
}

/// A blob's header: frames, start, codec id and the codec's body.
pub fn split_blob(blob: &[u8]) -> Result<(u32, Start, u8, &[u8])> {
    let mut i = 0;
    let frames = get_varint(blob, &mut i)?;
    if frames > MAX_FRAMES as u64 {
        bail!("proof claims {frames} frames");
    }
    let start = proof_start(blob)?;
    let id = *blob.get(i + 1).ok_or_else(|| anyhow!("truncated proof"))?;
    Ok((frames as u32, start, id, &blob[i + 2..]))
}

/// Just the start of a proof blob (cheap: no decompression).
pub fn proof_start(blob: &[u8]) -> Result<Start> {
    let mut i = 0;
    get_varint(blob, &mut i)?;
    match blob.get(i) {
        Some(0) => Ok(Start::Parent),
        Some(1) => Ok(Start::PowerOn),
        Some(x) => bail!("unknown start {x}"),
        None => bail!("truncated proof"),
    }
}

/// Decodes a proof blob; rejects more than `max_frames` frames or more inputs
/// than frames.
pub fn decode_proof(blob: &[u8], max_frames: usize) -> Result<Proof> {
    let mut i = 0;
    let frames = get_varint(blob, &mut i)?;
    if frames as usize > max_frames.min(MAX_FRAMES) {
        bail!("proof claims {frames} frames, over {max_frames}");
    }
    let start = match blob.get(i) {
        Some(0) => Start::Parent,
        Some(1) => Start::PowerOn,
        Some(x) => bail!("unknown start {x}"),
        None => bail!("truncated proof"),
    };
    let inputs = decode(&blob[i + 1..], frames as usize)?;
    Ok(Proof { frames: frames as u32, start, inputs })
}

pub fn pad_bytes(p: &GamePad) -> [u8; 4] {
    let b = p.button.to_le_bytes();
    [b[0], b[1], p.stick_x as u8, p.stick_y as u8]
}

pub fn pad_from(c: &[u8]) -> GamePad {
    GamePad::new(u16::from_le_bytes([c[0], c[1]]), c[2] as i8, c[3] as i8)
}

/// Codec 0: 4 bytes per frame (buttons u16 LE, stick x i8, stick y i8).
pub struct Raw;

impl Codec for Raw {
    fn id(&self) -> u8 {
        0
    }
    fn name(&self) -> &'static str {
        "raw"
    }
    fn encode(&self, pads: &[GamePad]) -> Vec<u8> {
        pads.iter().flat_map(pad_bytes).collect()
    }
    fn decode(&self, body: &[u8], max_frames: usize) -> Result<Vec<GamePad>> {
        if body.len() % 4 != 0 {
            bail!("raw proof length {} is not a multiple of 4", body.len());
        }
        if body.len() / 4 > max_frames {
            bail!("proof has {} frames, over {max_frames}", body.len() / 4);
        }
        Ok(body.chunks_exact(4).map(pad_from).collect())
    }
}

/// Codec 1: runs of identical pads, each `varint(run length) + 4 pad bytes`.
/// Players hold inputs for many frames, so this is often several times
/// smaller than raw. A placeholder for real compression.
pub struct RunLength;

pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub(crate) fn get_varint(b: &[u8], i: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*i).ok_or_else(|| anyhow!("truncated varint"))?;
        *i += 1;
        v |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            // Canonical only: no trailing zero groups, so each proof has one encoding.
            if byte == 0 && shift > 0 {
                bail!("non-canonical varint");
            }
            return Ok(v);
        }
    }
    bail!("varint too long")
}

impl Codec for RunLength {
    fn id(&self) -> u8 {
        1
    }
    fn name(&self) -> &'static str {
        "rle"
    }
    fn encode(&self, pads: &[GamePad]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < pads.len() {
            let mut j = i + 1;
            while j < pads.len() && pads[j] == pads[i] {
                j += 1;
            }
            put_varint(&mut out, (j - i) as u64);
            out.extend(pad_bytes(&pads[i]));
            i = j;
        }
        out
    }
    fn decode(&self, body: &[u8], max_frames: usize) -> Result<Vec<GamePad>> {
        let mut pads = Vec::new();
        let mut i = 0;
        let mut last: Option<GamePad> = None;
        while i < body.len() {
            let n = get_varint(body, &mut i)? as usize;
            if n == 0 {
                bail!("zero-length run");
            }
            let c = body.get(i..i + 4).ok_or_else(|| anyhow!("truncated run"))?;
            i += 4;
            let p = pad_from(c);
            // Canonical: adjacent runs must differ.
            if last == Some(p) {
                bail!("adjacent runs repeat the same pad");
            }
            last = Some(p);
            if pads.len() + n > max_frames {
                bail!("proof expands past {max_frames} frames");
            }
            pads.extend(std::iter::repeat_n(p, n));
        }
        Ok(pads)
    }
}

/// Codec 2: the raw pads (codec 0's body) in an xz (LZMA2) stream. About
/// 0.6 bytes per frame on real runs, against 1.4 for run-length; canonical
/// proofs (poked frames repeat the previous input) do better still.
pub struct Xz;

/// liblzma decoder memory limit (bytes); must stay above what a preset-9 (`-9e`) stream needs (~65 MiB).
#[cfg(not(target_arch = "wasm32"))]
const XZ_DECODE_MEMLIMIT: u64 = 72 << 20;

#[cfg(not(target_arch = "wasm32"))]
impl Codec for Xz {
    fn id(&self) -> u8 {
        2
    }
    fn name(&self) -> &'static str {
        "xz"
    }
    fn encode(&self, pads: &[GamePad]) -> Vec<u8> {
        use std::io::Write;
        let raw = Raw.encode(pads);
        let mut e = xz2::write::XzEncoder::new(Vec::new(), 9 | 0x8000_0000); // preset 9, extreme
        e.write_all(&raw).expect("xz encode");
        e.finish().expect("xz encode")
    }
    fn decode(&self, body: &[u8], max_frames: usize) -> Result<Vec<GamePad>> {
        use std::io::Read;
        // Never expand past the limit (a tiny stream could claim gigabytes).
        let cap = max_frames.saturating_mul(4).saturating_add(1);
        let mut raw = Vec::new();
        // Decoder memory limit: 72 MiB (consensus: a stream that needs more doesn't decode, so its
        // proof_id is the "sm64bad!" one). Honest streams are preset 9 (64 MiB dictionary, ~65 MiB to decode);
        // a crafted header asking for a bigger dictionary fails instead of allocating up to ~4 GiB.
        let stream = xz2::stream::Stream::new_stream_decoder(XZ_DECODE_MEMLIMIT, 0).map_err(|e| anyhow!("xz: {e}"))?;
        xz2::read::XzDecoder::new_stream(body, stream).take(cap as u64).read_to_end(&mut raw).map_err(|e| anyhow!("xz: {e}"))?;
        if raw.len() >= cap {
            bail!("proof expands past {max_frames} frames");
        }
        Raw.decode(&raw, max_frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<GamePad> {
        let mut v = vec![GamePad::new(0, 0, 0); 50];
        v.extend(vec![GamePad::new(0x8000, -80, 80); 7]);
        v.push(GamePad::new(0x1234, 3, -4));
        v.extend(vec![GamePad::new(0, 0, 0); 300]);
        v
    }

    #[test]
    fn every_codec_round_trips() {
        let pads = sample();
        for c in CODECS {
            let blob = encode_with(*c, &pads);
            assert_eq!(blob[0], c.id());
            assert_eq!(decode(&blob, 10_000).unwrap(), pads, "{}", c.name());
            assert!(decode(&blob, pads.len() - 1).is_err(), "{} limit", c.name());
        }
        assert_eq!(decode(&encode_with(&Raw, &[]), 10).unwrap(), vec![]);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn xz_decoder_memory_is_limited() {
        // An honest preset-9 (-9e, 64 MiB dictionary) stream still decodes.
        let pads = sample();
        let blob = encode_with(&Xz, &pads);
        assert_eq!(decode(&blob, 10_000).unwrap(), pads);
        // The LZMA2 dictionary-size byte (block header, CRC32 fixed up): dict = (2 | (b & 1)) << (b / 2 + 11).
        let with_dict = |b: u8| {
            let mut body = blob[1..].to_vec();
            assert_eq!(body[12 + 2], 0x21, "LZMA2 filter id");
            body[12 + 4] = b;
            let mut crc = !0u32;
            for &x in &body[12..12 + 8] {
                crc ^= x as u32;
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
                }
            }
            body[12 + 8..12 + 12].copy_from_slice(&(!crc).to_le_bytes());
            body
        };
        assert_eq!(blob[1 + 12 + 4], 0x1c, "the encoder's preset 9: a 64 MiB dictionary");
        assert!(Xz.decode(&with_dict(0x1c), 10_000).is_ok(), "64 MiB (honest preset 9): decodes under 72 MiB");
        for (b, what) in [(0x1d, "96 MiB"), (0x1e, "128 MiB"), (0x28, "4 GiB")] {
            let err = format!("{:#}", Xz.decode(&with_dict(b), 10_000).unwrap_err());
            assert!(err.contains("memory"), "{what}: {err}");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_stream_needing_more_than_72_mib_has_the_bad_proof_id() {
        use crate::proof::{Proof, Start, encode_proof_with};
        let pr = Proof { frames: sample().len() as u32, start: Start::Parent, inputs: sample() };
        let good = encode_proof_with(&Xz, &pr);
        // The proof blob: varint(frames) start codec body. Patch the body's dictionary byte to 96 MiB.
        let off = good.len() - Xz.encode(&pr.inputs).len();
        let mut bad = good.clone();
        assert_eq!(bad[off + 12 + 2], 0x21);
        bad[off + 12 + 4] = 0x1d;
        let mut crc = !0u32;
        for &x in &bad[off + 12..off + 12 + 8] {
            crc ^= x as u32;
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
            }
        }
        bad[off + 12 + 8..off + 12 + 12].copy_from_slice(&(!crc).to_le_bytes());
        assert!(decode_proof(&bad, 10_000).is_err(), "needs 96 MiB: refused");
        assert_ne!(crate::proof_id::proof_id(&bad), crate::proof_id::proof_id(&good), "so it can't share the run's block id");
        let mut h = <sha3::Keccak256 as sha3::Digest>::new();
        sha3::Digest::update(&mut h, b"sm64bad!");
        sha3::Digest::update(&mut h, &bad);
        let bad_id: [u8; 32] = sha3::Digest::finalize(h).into();
        assert_eq!(crate::proof_id::proof_id(&bad), bad_id, "the sm64bad id");
    }

    #[test]
    fn best_is_smallest_and_rle_wins_on_held_inputs() {
        let pads = sample();
        let best = encode_best(&pads);
        assert_eq!(best[0], RunLength.id());
        assert!(best.len() < 30);
    }

    #[test]
    fn rejects_malformed() {
        assert!(decode(&[], 10).is_err());
        assert!(decode(&[9, 1, 2], 10).is_err());
        assert!(decode(&[0, 1, 2, 3], 10).is_err());
        assert!(decode(&[1, 0, 0, 0, 0, 0], 10).is_err()); // zero run
        assert!(decode(&[1, 0x80, 0x00, 0, 0, 0, 0], 10).is_err()); // non-canonical varint
        assert!(decode(&[1, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0], 10).is_err()); // repeated run
    }
}
