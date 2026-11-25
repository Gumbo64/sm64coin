//! Codec negotiation (v0.4, node policy, layer 3): with committed inputs (commit.rs) a proof's
//! encoding is transport, so each peer says which codecs it decodes (`NOTIFY_SM64_CODECS`, levin
//! `BC_COMMANDS_POOL_BASE + 13`: a list of `(codec id u8, codec version u8)` pairs) and a relaying
//! node sends every peer an encoding it decodes, transcoding when needed. The block id doesn't
//! change: it commits the header's input hash. docs/V0.4-DESIGN.md section 1.5.
//!
//! - Every v0.4 node decodes the **baseline** (0 raw, 1 run-length, 2 xz: stateless, so any node
//!   that knows a run's inputs can always produce them). A peer that advertised nothing gets the
//!   baseline.
//! - A peer's set ([`Mask`]) holds the ids it advertised **at our version** of them (a frozen
//!   decoder per `(id, version)`; ids are never reused), plus the baseline.
//! - Transcoding ([`transcode`]) goes to the smallest **stateless** encoding the peer decodes:
//!   encoding a state codec needs the replay's game states, decoding needs the inputs only. A
//!   state-coded blob's inputs come from the caller (the daemon's replay of the block,
//!   `Played::wanted`) and are checked against the committed hash.
//! - What this node decodes is policy too ([`set_local`], `SM64POW_CODECS=0,1,2,...` at start-up):
//!   a blob in a codec it doesn't decode is a bad encoding (commit.rs), never an invalid block.
//!
//! With committed inputs off (v0.3's layout) the encoding is consensus: nothing is transcoded.
//!
//! Read next: commit.rs.

use crate::commit;
use crate::proof::{self, Proof};
use anyhow::{Result, bail};
use std::sync::RwLock;

/// The version of every codec this release ships (each id's decoder is frozen at version 1).
pub const VERSION: u8 = 1;
/// The stateless codecs every v0.4 node decodes.
pub const BASELINE: [u8; 3] = [0, 1, 2];
/// The most pairs an advert may list (one per id).
pub const MAX_PAIRS: usize = 256;

/// A set of codec ids (one bit per id).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mask(pub [u8; 32]);

impl Mask {
    pub const EMPTY: Mask = Mask([0; 32]);
    pub fn baseline() -> Mask {
        let mut m = Mask::EMPTY;
        for id in BASELINE {
            m.insert(id);
        }
        m
    }
    pub fn insert(&mut self, id: u8) {
        self.0[id as usize / 8] |= 1 << (id % 8);
    }
    pub fn has(&self, id: u8) -> bool {
        self.0[id as usize / 8] & (1 << (id % 8)) != 0
    }
    pub fn ids(&self) -> impl Iterator<Item = u8> + '_ {
        (0..=255u8).filter(|&i| self.has(i))
    }
}

/// Every codec this build can decode: the stateless ones, and the state codecs (statecodec.rs,
/// committed inputs only: with v0.3's layout they are consensus and the canonical check applies).
pub fn shipped() -> Mask {
    let mut m = Mask::baseline();
    for c in proof::CODECS {
        m.insert(c.id());
    }
    if crate::statecodec::state_codec_enabled() {
        for id in 3..=255u8 {
            if crate::statecodec::state_codec(id).is_some() {
                m.insert(id);
            }
        }
    }
    m
}

static LOCAL: RwLock<Option<Mask>> = RwLock::new(None);

/// What this node decodes (and advertises): [`shipped`] unless narrowed by [`set_local`] or
/// `SM64POW_CODECS` (read once).
pub fn local() -> Mask {
    if let Some(m) = *LOCAL.read().unwrap() {
        return m;
    }
    let m = match std::env::var("SM64POW_CODECS") {
        Ok(s) => parse_list(&s).unwrap_or_else(|_| shipped()),
        Err(_) => shipped(),
    };
    *LOCAL.write().unwrap() = Some(m);
    m
}

/// Narrows what this node decodes to `ids` (shipped ones only); the baseline always stays.
pub fn set_local(ids: &[u8]) {
    let s = shipped();
    let mut m = Mask::baseline();
    for &id in ids {
        if s.has(id) {
            m.insert(id);
        }
    }
    *LOCAL.write().unwrap() = Some(m);
}

fn parse_list(s: &str) -> Result<Mask> {
    let ids: Vec<u8> = s.split(',').filter(|x| !x.trim().is_empty()).map(|x| x.trim().parse::<u8>()).collect::<Result<_, _>>()?;
    let sh = shipped();
    let mut m = Mask::baseline();
    for id in ids {
        if sh.has(id) {
            m.insert(id);
        }
    }
    Ok(m)
}

/// Whether this node decodes codec `id`.
pub fn decodes(id: u8) -> bool {
    local().has(id)
}

/// This node's advert: `(id, version)` for every codec it decodes.
pub fn advert() -> Vec<u8> {
    local().ids().flat_map(|id| [id, VERSION]).collect()
}

/// A peer's set from its advert: the ids it lists at our version (that we ship), plus the
/// baseline. A malformed advert (odd length, too long) gives the baseline.
pub fn peer_mask(advert: &[u8]) -> Mask {
    let mut m = Mask::baseline();
    if advert.len() % 2 != 0 || advert.len() > 2 * MAX_PAIRS {
        return m;
    }
    let sh = shipped();
    for p in advert.chunks_exact(2) {
        if p[1] == VERSION && sh.has(p[0]) {
            m.insert(p[0]);
        }
    }
    m
}

/// The codec id of a committed blob (None: the header or the encoding is missing).
pub fn codec_of(blob: &[u8]) -> Option<u8> {
    commit::split(blob).ok().and_then(|(_, enc)| enc.first().copied())
}

/// What sending `blob` to a peer with set `peer` needs: 0 nothing (send it as is), 1 a stateless
/// transcode (no inputs needed), 2 a transcode from the run's inputs (state-coded: pass them to
/// [`transcode`]). With committed inputs off: always 0.
pub fn need(blob: &[u8], peer: &Mask) -> u8 {
    if !crate::features::committed() {
        return 0;
    }
    match codec_of(blob) {
        Some(id) if peer.has(id) => 0,
        Some(id) if proof::codec(id).is_some() => 1,
        _ => 2,
    }
}

/// `blob` for a peer with set `peer`: None when it can be sent as is, else the same committed
/// header (the same block id) over the smallest stateless encoding the peer decodes. `inputs`:
/// the run's inputs (4 bytes per non-poked frame) when the blob is state-coded; they must hash to
/// the committed hash.
pub fn transcode(blob: &[u8], inputs: Option<&[u8]>, peer: &Mask) -> Result<Option<Vec<u8>>> {
    if need(blob, peer) == 0 {
        return Ok(None);
    }
    let (h, enc) = commit::split(blob)?;
    let pads = match proof::codec(enc.first().copied().unwrap_or(0xff)) {
        Some(_) => proof::decode(enc, h.frames as usize)?,
        None => match inputs {
            Some(raw) if raw.len() % 4 == 0 => raw.chunks_exact(4).map(proof::pad_from).collect(),
            Some(_) => bail!("inputs are not whole pads"),
            None => bail!("a state-coded blob needs the run's inputs to transcode"),
        },
    };
    let pr = Proof { frames: h.frames, start: h.start, inputs: pads };
    if commit::proof_hash(&pr) != h.input_hash {
        bail!("the inputs do not hash to the committed hash");
    }
    let best = proof::CODECS.iter().filter(|c| peer.has(c.id())).map(|c| proof::encode_with(*c, &pr.inputs)).min_by_key(|b| b.len());
    let enc = best.unwrap_or_else(|| proof::encode_with(&proof::Raw, &pr.inputs));
    Ok(Some(commit::join(&h, &enc)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proof::{RunLength, Start, Xz};
    use sm64_binds::GamePad;

    fn run() -> Proof {
        let mut v = vec![GamePad::new(0, 0, 0); 200];
        v.extend((0..50).map(|i| GamePad::new(0x8000, i as i8, -(i as i8))));
        Proof { frames: 260, start: Start::Parent, inputs: v }
    }

    #[test]
    fn adverts_and_masks() {
        let b = Mask::baseline();
        assert!(b.has(0) && b.has(1) && b.has(2) && !b.has(3) && !b.has(255));
        assert_eq!(peer_mask(&[]), b, "nothing advertised: the baseline");
        assert_eq!(peer_mask(&[5]), b, "malformed: the baseline");
        assert_eq!(peer_mask(&vec![0; 2 * MAX_PAIRS + 2]), b, "too long: the baseline");
        let m = peer_mask(&[4, VERSION, 5, VERSION + 1, 0xee, VERSION]);
        assert_eq!(m.has(4), shipped().has(4));
        assert!(!m.has(5), "another version of 5: not ours");
        assert!(!m.has(0xee), "an id we don't ship");
        let ours = peer_mask(&advert());
        assert_eq!(ours, local());
        for id in BASELINE {
            assert!(advert().chunks(2).any(|p| p == [id, VERSION]));
        }
        // Narrowing what this node decodes (one test: the set is process-wide).
        set_local(&[]);
        assert_eq!(local(), Mask::baseline());
        assert!(decodes(2) && !decodes(4));
        assert_eq!(peer_mask(&advert()), Mask::baseline());
        set_local(&[4, 0xee]);
        assert_eq!(local().has(4), shipped().has(4));
        assert!(!local().has(0xee));
        *LOCAL.write().unwrap() = Some(shipped());
    }

    #[test]
    fn transcoding_keeps_the_header_and_the_id() {
        let pr = run();
        let xz = commit::commit_with(&Xz, &pr);
        let mut only_rle = Mask::EMPTY;
        only_rle.insert(1);
        assert_eq!(need(&xz, &Mask::baseline()), 0);
        assert_eq!(transcode(&xz, None, &Mask::baseline()).unwrap(), None);
        assert_eq!(need(&xz, &only_rle), 1);
        let t = transcode(&xz, None, &only_rle).unwrap().unwrap();
        assert_eq!(codec_of(&t), Some(1));
        assert_eq!(commit::proof_id(&t), commit::proof_id(&xz));
        assert!(matches!(commit::open(&t, 1000, false), Ok(commit::Opened::Run(_, ref p)) if *p == pr));
        // A state-coded blob (any body under a good header): needs the inputs, which must match.
        let state = commit::join(&commit::header_of(&pr), &[5, 1, 2, 3]);
        assert_eq!(need(&state, &Mask::baseline()), 2);
        assert!(transcode(&state, None, &Mask::baseline()).is_err());
        let raw: Vec<u8> = pr.inputs.iter().flat_map(proof::pad_bytes).collect();
        let t = transcode(&state, Some(&raw), &Mask::baseline()).unwrap().unwrap();
        assert_eq!(commit::proof_id(&t), commit::proof_id(&state));
        assert_eq!(t, commit::commit(&pr), "the smallest stateless encoding");
        let mut wrong = raw.clone();
        wrong[0] ^= 1;
        assert!(transcode(&state, Some(&wrong), &Mask::baseline()).is_err());
        assert!(transcode(&state, Some(&raw[..raw.len() - 1]), &Mask::baseline()).is_err());
        // A blob whose stateless encoding is bad: an error, never a wrong blob.
        let lie = commit::join(&commit::header_of(&pr), &proof::encode_with(&RunLength, &pr.inputs[1..]));
        assert!(transcode(&lie, None, &only_rle).is_err() || need(&lie, &only_rle) == 0);
        let mut xz_only = Mask::EMPTY;
        xz_only.insert(2);
        assert!(transcode(&lie, None, &xz_only).is_err());
    }
}
