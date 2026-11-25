//! Committed inputs (v0.4, layer 1): the proof's header commits the hash of
//! the run's canonical inputs, and the encoding becomes transport.
//!
//! ```text
//! sm64_proof = varint(frames) || start || input_hash (32) || encoding
//! encoding   = codec id || codec body           (not consensus: any encoding of the inputs will do)
//! input_hash = keccak256("sm64run2" || frames u32 LE || start u8 || 4 bytes per non-poked input)
//! ```
//!
//! `input_hash` is byte for byte v0.3's `proof_id` of a stateless proof, so
//! the proof_id vectors carry over. In v0.4 [`proof_id`] is the committed
//! hash, read from the header without decoding anything; the block id and the
//! signature use it (unchanged in form). A blob whose decoded inputs don't
//! hash to the committed hash is a **bad encoding** ([`Fault::BadEncoding`]):
//! nothing is said about the block id, which every good encoding shares; only
//! what the committed header says (its length, its start) and what the decoded
//! run does once it matches the hash can make a block **invalid**.
//! docs/V0.4-DESIGN.md section 1.
//!
//! Stateless codecs (0-2) are checked here, before any replay ([`open`]);
//! state codecs (3-5) are decoded and hashed during the replay
//! (`replay::replay_committed`).
//!
//! Read next: replay.rs (`replay_committed`), then chain.rs (`verify_committed_block`).

use crate::proof::{self, Codec, Proof, Start};
use anyhow::{Result, anyhow, bail};
use sha3::{Digest, Keccak256};
use sm64_binds::GamePad;

/// Domain tag of the input hash (v0.3's stateless proof_id).
pub const INPUT_TAG: &[u8; 8] = b"sm64run2";
/// Domain tag of the id of a blob whose header doesn't parse.
pub const BAD_TAG: &[u8; 8] = b"sm64bad!";

/// The committed part of a proof blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub frames: u32,
    pub start: Start,
    pub input_hash: [u8; 32],
}

/// Hashes a run's inputs as they are decoded (one pad per non-poked frame).
pub struct InputHasher(Keccak256);

impl InputHasher {
    pub fn new(frames: u32, start: Start) -> Self {
        let mut h = Keccak256::new();
        h.update(INPUT_TAG);
        h.update(frames.to_le_bytes());
        h.update([start as u8]);
        InputHasher(h)
    }
    pub fn pad(&mut self, p: &GamePad) {
        self.0.update(proof::pad_bytes(p));
    }
    pub fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

/// The input hash of a run: its length, its start and the inputs of its non-poked frames.
pub fn input_hash(frames: u32, start: Start, inputs: &[GamePad]) -> [u8; 32] {
    let mut h = InputHasher::new(frames, start);
    for p in inputs {
        h.pad(p);
    }
    h.finish()
}

/// The input hash of a decoded proof.
pub fn proof_hash(pr: &Proof) -> [u8; 32] {
    input_hash(pr.frames, pr.start, &pr.inputs)
}

/// Splits a committed blob into its header and its encoding (`codec id || body`, possibly empty).
pub fn split(blob: &[u8]) -> Result<(Header, &[u8])> {
    let mut i = 0;
    let frames = proof::get_varint(blob, &mut i)?;
    if frames > proof::MAX_FRAMES as u64 {
        bail!("proof claims {frames} frames");
    }
    let start = match blob.get(i) {
        Some(0) => Start::Parent,
        Some(1) => Start::PowerOn,
        Some(x) => bail!("unknown start {x}"),
        None => bail!("truncated proof"),
    };
    let hash: [u8; 32] = blob.get(i + 1..i + 33).ok_or_else(|| anyhow!("truncated proof header"))?.try_into().unwrap();
    Ok((Header { frames: frames as u32, start, input_hash: hash }, &blob[i + 33..]))
}

/// The committed header alone.
pub fn header(blob: &[u8]) -> Result<Header> {
    split(blob).map(|(h, _)| h)
}

/// What the block id (and the signature) commit to: the committed input hash, read from the
/// header with no decoding; a blob whose header doesn't parse gets `keccak256("sm64bad!" || blob)`,
/// so it never shares an id with a run.
pub fn proof_id(blob: &[u8]) -> [u8; 32] {
    match split(blob) {
        Ok((h, _)) => h.input_hash,
        Err(_) => {
            let mut k = Keccak256::new();
            k.update(BAD_TAG);
            k.update(blob);
            k.finalize().into()
        }
    }
}

/// The committed blob of `header` carried by `encoding` (`codec id || body`).
pub fn join(header: &Header, encoding: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(38 + encoding.len());
    proof::put_varint(&mut out, header.frames as u64);
    out.push(header.start as u8);
    out.extend_from_slice(&header.input_hash);
    out.extend_from_slice(encoding);
    out
}

/// The header of a decoded proof.
pub fn header_of(pr: &Proof) -> Header {
    Header { frames: pr.frames, start: pr.start, input_hash: proof_hash(pr) }
}

/// A proof, committed, in stateless codec `c`.
pub fn commit_with(c: &dyn Codec, pr: &Proof) -> Vec<u8> {
    join(&header_of(pr), &proof::encode_with(c, &pr.inputs))
}

/// A proof, committed, in the smallest stateless codec.
pub fn commit(pr: &Proof) -> Vec<u8> {
    join(&header_of(pr), &proof::encode_best(&pr.inputs))
}

/// A v0.3-layout blob (`varint(frames) || start || codec || body`, what the codecs and the replay
/// read) with `input_hash` committed: inserts the hash after the start byte. For sealing a
/// state-coded run (`statecodec::encode_proof_state` builds the v0.3 layout).
pub fn commit_layout(v03: &[u8], input_hash: &[u8; 32]) -> Result<Vec<u8>> {
    let (frames, start, _, _) = proof::split_blob(v03)?;
    let at = varint_len(frames as u64) + 1;
    Ok(join(&Header { frames, start, input_hash: *input_hash }, &v03[at..]))
}

/// The v0.3 layout of a committed blob (drops the hash): what the existing decoders and replay read.
pub fn layout(blob: &[u8]) -> Result<Vec<u8>> {
    let (h, enc) = split(blob)?;
    let mut out = Vec::with_capacity(6 + enc.len());
    proof::put_varint(&mut out, h.frames as u64);
    out.push(h.start as u8);
    out.extend_from_slice(enc);
    Ok(out)
}

/// The same committed run in another encoding (transport: the id doesn't change).
pub fn reencode(blob: &[u8], encoding: &[u8]) -> Result<Vec<u8>> {
    Ok(join(&header(blob)?, encoding))
}

fn varint_len(mut v: u64) -> usize {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

/// Why a committed blob doesn't verify.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The committed run (or its header) breaks a rule: every encoding of it gets this verdict.
    Invalid(String),
    /// This blob doesn't decode to the committed inputs: says nothing about the block id.
    BadEncoding(String),
}

/// A committed blob, opened: its header and either the decoded (and hash-checked) run, or a
/// state codec to decode during the replay.
#[derive(Debug)]
pub enum Opened<'a> {
    /// A stateless codec: decoded, and its inputs hash to the committed hash.
    Run(Header, Proof),
    /// A state codec (`id`, `body`): decoded and hash-checked during the replay.
    State(Header, u8, &'a [u8]),
}

/// Opens a committed blob, without the game: the header must parse (else Invalid: its id is the
/// "sm64bad!" one, unique to it) and claim at least one frame and at most `max_frames` (the
/// committed length: Invalid); a stateless encoding must decode to inputs that hash to the
/// committed hash (else BadEncoding). `state_ok`: whether state codecs are accepted (else a
/// state-coded blob is a BadEncoding: another node may send this run in a codec we decode).
pub fn open(blob: &[u8], max_frames: u32, state_ok: bool) -> std::result::Result<Opened<'_>, Fault> {
    let (h, enc) = split(blob).map_err(|e| Fault::Invalid(format!("bad proof header: {e}")))?;
    if h.frames == 0 {
        return Err(Fault::Invalid("empty proof".into()));
    }
    if h.frames > max_frames {
        return Err(Fault::Invalid(format!("{} frames exceeds the limit of {max_frames}", h.frames)));
    }
    let (&cid, body) = enc.split_first().ok_or_else(|| Fault::BadEncoding("no encoding".into()))?;
    if state_ok && crate::statecodec::state_codec_enabled() && crate::statecodec::state_codec(cid).is_some() && crate::codecs::decodes(cid) {
        return Ok(Opened::State(h, cid, body));
    }
    let inputs = proof::decode(enc, h.frames as usize).map_err(|e| Fault::BadEncoding(format!("{e:#}")))?;
    let pr = Proof { frames: h.frames, start: h.start, inputs };
    if proof_hash(&pr) != h.input_hash {
        return Err(Fault::BadEncoding("the encoding does not decode to the committed inputs".into()));
    }
    Ok(Opened::Run(h, pr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proof::{Raw, RunLength, Xz};

    fn sample() -> Proof {
        let mut v = vec![GamePad::new(0, 0, 0); 30];
        v.extend(vec![GamePad::new(0x8000, -80, 80); 9]);
        v.push(GamePad::new(0x1234, 3, -4));
        Proof { frames: 48, start: Start::Parent, inputs: v }
    }

    #[test]
    fn the_input_hash_is_v03s_stateless_proof_id() {
        let pr = sample();
        for c in [&Raw as &dyn Codec, &RunLength, &Xz] {
            assert_eq!(proof_hash(&pr), crate::proof_id::proof_id(&proof::encode_proof_with(c, &pr)), "{}", c.name());
        }
    }

    #[test]
    fn proof_id_is_the_header_whatever_the_encoding() {
        let pr = sample();
        let blobs: Vec<Vec<u8>> = [&Raw as &dyn Codec, &RunLength, &Xz].iter().map(|c| commit_with(*c, &pr)).collect();
        for b in &blobs {
            assert_eq!(proof_id(b), proof_hash(&pr));
            assert!(matches!(open(b, 100, true), Ok(Opened::Run(h, ref p)) if h == header_of(&pr) && *p == pr));
        }
        // Garbage after a good header: the same id, a bad encoding (never Invalid).
        let mut junk = join(&header_of(&pr), &[1, 0x55, 0x55]);
        assert_eq!(proof_id(&junk), proof_hash(&pr));
        assert!(matches!(open(&junk, 100, true), Err(Fault::BadEncoding(_))));
        junk.truncate(junk.len() - 3);
        assert!(matches!(open(&junk, 100, true), Err(Fault::BadEncoding(_))), "no encoding at all");
        // Another run's encoding under this header: decodes fine, wrong hash.
        let other = Proof { inputs: vec![GamePad::new(1, 0, 0); 40], ..pr.clone() };
        let lie = join(&header_of(&pr), &proof::encode_with(&RunLength, &other.inputs));
        assert!(matches!(open(&lie, 100, true), Err(Fault::BadEncoding(_))));
        // An unknown codec id: bad encoding (codecs are node-side).
        assert!(matches!(open(&join(&header_of(&pr), &[0xee, 1, 2]), 100, true), Err(Fault::BadEncoding(_))));
    }

    #[test]
    fn the_committed_header_decides_invalid() {
        let pr = sample();
        let b = commit(&pr);
        assert!(matches!(open(&b, 47, true), Err(Fault::Invalid(_))), "over the limit: the committed length");
        let empty = join(&Header { frames: 0, start: Start::Parent, input_hash: [0; 32] }, &[0]);
        assert!(matches!(open(&empty, 100, true), Err(Fault::Invalid(_))));
        for n in 0..34 {
            let t = &b[..n];
            assert!(matches!(open(t, 100, true), Err(Fault::Invalid(_))), "truncated header {n}");
            let mut k = Keccak256::new();
            k.update(BAD_TAG);
            k.update(t);
            let bad: [u8; 32] = k.finalize().into();
            assert_eq!(proof_id(t), bad);
        }
        let mut s = b.clone();
        s[1] = 2; // the start byte
        assert!(matches!(open(&s, 100, true), Err(Fault::Invalid(_))));
    }

    #[test]
    fn layouts_round_trip_and_reencode_keeps_the_id() {
        let pr = sample();
        let old = proof::encode_proof_with(&Xz, &pr);
        let c = commit_layout(&old, &proof_hash(&pr)).unwrap();
        assert_eq!(layout(&c).unwrap(), old);
        assert_eq!(c, commit_with(&Xz, &pr));
        let rle = reencode(&c, &proof::encode_with(&RunLength, &pr.inputs)).unwrap();
        assert_eq!(proof_id(&rle), proof_id(&c));
        assert_ne!(rle, c);
        // A long run: the varint takes more than one byte.
        let long = Proof { frames: 30_000, start: Start::PowerOn, inputs: vec![GamePad::new(0, 1, 1); 29_000] };
        let c = commit_with(&RunLength, &long);
        assert_eq!(header(&c).unwrap(), header_of(&long));
        assert_eq!(layout(&c).unwrap(), proof::encode_proof_with(&RunLength, &long));
    }

    #[test]
    fn the_hash_commits_length_start_and_every_input() {
        let pr = sample();
        let h = proof_hash(&pr);
        assert_ne!(h, proof_hash(&Proof { frames: 49, ..pr.clone() }));
        assert_ne!(h, proof_hash(&Proof { start: Start::PowerOn, ..pr.clone() }));
        let mut p2 = pr.clone();
        p2.inputs[17] = GamePad::new(0, 0, 1);
        assert_ne!(h, proof_hash(&p2));
    }
}
