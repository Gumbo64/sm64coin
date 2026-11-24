//! The state-aware codec on real runs: round trips from power-on and parent
//! starts, canonical encoding, determinism, bounds.

use sha3::{Digest, Keccak256};
use sm64pow::chain::{BlockVerdict, Parent, played_block_opts, rebuild_block_opts, verify_block_opts};
use sm64pow::proof::{self, Proof, Start, pad_from};
use sm64pow::sm64_binds::{GamePad, SaveState};
use sm64pow::statecodec::{self, StateCtx1, encode_proof_state, state_after, state_proof_id};
use sm64pow::{Seed, params};

fn init() -> bool {
    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    match std::fs::read(&rom) {
        Ok(b) => {
            let _ = sm64pow::init(b);
            true
        }
        Err(_) => {
            eprintln!("skipping: no ROM at {rom} (set SM64COIN_ROM)");
            false
        }
    }
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

/// Block 1 of an old chain: power-on to a star.
fn power_on() -> (Seed, Proof) {
    let j: serde_json::Value = serde_json::from_str(include_str!("fixtures/power_on_star.json")).unwrap();
    let seed = Seed::from_hex(j["seed_hash"].as_str().unwrap()).unwrap();
    let blob = hex(j["proof_hex"].as_str().unwrap());
    (seed, proof::decode_proof(&blob, 100_000).unwrap())
}

/// A block on top of another (a parent start): the parent (a power-on
/// proof), and the block's seed and proof.
fn parent_start() -> ((Seed, Proof), (Seed, Proof)) {
    let j: serde_json::Value = serde_json::from_str(include_str!("fixtures/block4_parent_start.json")).unwrap();
    let pads = |v: &serde_json::Value| -> Vec<GamePad> { hex(v.as_str().unwrap()).chunks_exact(4).map(pad_from).collect() };
    let mk = |v: &serde_json::Value, start| (Seed::from_hex(v["seed_hash"].as_str().unwrap()).unwrap(), Proof { frames: v["frames"].as_u64().unwrap() as u32, start, inputs: pads(&v["inputs"]) });
    (mk(&j["parent"], Start::PowerOn), mk(&j, Start::Parent))
}

fn id(n: u8) -> [u8; 32] {
    [n; 32]
}

fn valid(v: &BlockVerdict) -> bool {
    matches!(v, BlockVerdict::Valid { .. })
}

fn invalid_with(v: BlockVerdict, what: &str) {
    match v {
        BlockVerdict::Invalid(w) => assert!(w.contains(what), "{w}"),
        v => panic!("{v:?}"),
    }
}

#[test]
fn power_on_round_trip_and_size() {
    if !init() {
        return;
    }
    let (seed, pr) = power_on();
    let blob = encode_proof_state(&StateCtx1, None, &seed, &params(1), &pr).unwrap();
    assert_eq!(&blob[..1].len(), &1);
    let v = verify_block_opts(&id(1), Parent::Genesis, &seed, 1, &blob, true).unwrap();
    let BlockVerdict::Valid { frames, progress, .. } = v else { panic!("{v:?}") };
    assert_eq!((frames, progress.stars), (pr.frames, 1));
    let xz = proof::encode_proof_with(&proof::Xz, &pr).len();
    eprintln!("power-on: state codec {} bytes, xz {xz}, best stateless {}", blob.len(), proof::encode_proof(&pr).len());
    assert!(blob.len() < xz);
    // Same played pads as the stateless proof.
    let plain = proof::encode_proof(&pr);
    assert_eq!(
        played_block_opts(&id(1), Parent::Genesis, &seed, &blob, true).unwrap().unwrap(),
        played_block_opts(&id(1), Parent::Genesis, &seed, &plain, true).unwrap().unwrap()
    );
    // Disabled (the default build): an unknown codec, like before the fork.
    invalid_with(verify_block_opts(&id(2), Parent::Genesis, &seed, 1, &blob, false).unwrap(), "unknown proof codec");
    assert_eq!(statecodec::state_codec_enabled(), cfg!(feature = "state-codec"));
}

#[test]
fn parent_start_round_trip() {
    if !init() {
        return;
    }
    let ((pseed, ppr), (seed, pr)) = parent_start();
    // The parent block (power-on, stateless codec) goes into the chain cache.
    let pblob = proof::encode_proof(&ppr);
    assert!(valid(&verify_block_opts(&id(41), Parent::Genesis, &pseed, 1, &pblob, true).unwrap()));
    let parent_played = played_block_opts(&id(41), Parent::Genesis, &pseed, &pblob, true).unwrap().unwrap();
    let ps = state_after(&parent_played).unwrap();
    let blob = encode_proof_state(&StateCtx1, Some(&ps), &seed, &params(1), &pr).unwrap();
    let xz = proof::encode_proof_with(&proof::Xz, &pr).len();
    eprintln!("parent start: state codec {} bytes, xz {xz}, best stateless {}", blob.len(), proof::encode_proof(&pr).len());
    assert!(blob.len() < xz);
    // Verified through the chain, on top of the cached parent.
    let parent = Parent::Block { id: id(41), gap_seconds: 1300 }; // past the limit's start offset (rules.rs start_halvings): the cap
    let v = verify_block_opts(&id(42), parent, &seed, 1, &blob, true).unwrap();
    let BlockVerdict::Valid { frames, progress, .. } = v else { panic!("{v:?}") };
    assert_eq!((frames, progress.stars), (pr.frames, 2));
    // Decoding by replaying agrees with the stateless replay of the same run.
    let played = sm64pow::played_from_proof_at(Some(&ps), &seed, &params(1), &pr).unwrap();
    assert_eq!(replay_state_blob(&blob, &ps, &seed), played);
    assert_eq!(played_block_opts(&id(42), parent, &seed, &blob, true).unwrap().unwrap(), played);
    // Canonical encoding applies to parent starts too.
    let mut longer = blob.clone();
    longer.push(0);
    invalid_with(verify_block_opts(&id(43), parent, &seed, 1, &longer, true).unwrap(), "canonical");
    // Without the parent's state the verdict is NeedParent, as for any codec.
    let unknown = Parent::Block { id: id(78), gap_seconds: 1300 }; // past the limit's start offset (rules.rs start_halvings): the cap
    assert_eq!(verify_block_opts(&id(44), unknown, &seed, 1, &blob, true).unwrap(), BlockVerdict::NeedParent);
}

/// Decodes a state blob by replaying from `parent` (what verification does).
fn replay_state_blob(blob: &[u8], parent: &SaveState, seed: &Seed) -> Vec<GamePad> {
    let (frames, start, cid, body) = proof::split_blob(blob).unwrap();
    assert_eq!((start, cid), (Start::Parent, StateCtx1.id_const()));
    let (mut g, _) = sm64pow::chain::mining_start(start, Some(parent), seed, &params(1)).unwrap();
    let mut dec = statecodec::state_codec(cid).unwrap().decoder(body);
    let mut out = vec![];
    for _ in 0..frames {
        let (pad, poked) = g.step_with(|f| Ok(sm64pow::game::from_pad(dec.pad(f)))).unwrap();
        if poked {
            dec.poked();
        }
        out.push(pad);
    }
    out
}

trait IdConst {
    fn id_const(&self) -> u8;
}
impl IdConst for StateCtx1 {
    fn id_const(&self) -> u8 {
        use sm64pow::statecodec::StateCodec;
        self.id()
    }
}

#[test]
fn chain_of_state_coded_blocks_and_canonical_encoding() {
    if !init() {
        return;
    }
    // Block 1 (power-on) then block 2 on top (parent start) are both state coded, through the chain
    // cache.
    let (seed, pr) = power_on();
    let b1 = encode_proof_state(&StateCtx1, None, &seed, &params(1), &pr).unwrap();
    assert!(valid(&verify_block_opts(&id(21), Parent::Genesis, &seed, 1, &b1, true).unwrap()));

    // Tampering: a trailing zero byte decodes the same run but is not canonical.
    let mut longer = b1.clone();
    longer.push(0);
    invalid_with(verify_block_opts(&id(22), Parent::Genesis, &seed, 1, &longer, true).unwrap(), "canonical");
    // Any other trailing byte, and any flipped bit, is rejected as well.
    let mut extra = b1.clone();
    extra.push(0x5a);
    assert!(!valid(&verify_block_opts(&id(23), Parent::Genesis, &seed, 1, &extra, true).unwrap()));
    for pos in [4, b1.len() / 3, b1.len() / 2, b1.len() - 1] {
        let mut t = b1.clone();
        t[pos] ^= 0x01;
        assert!(!valid(&verify_block_opts(&id(24), Parent::Genesis, &seed, 1, &t, true).unwrap()), "flip at {pos}");
    }
    // A truncated body decodes to another run (the last inputs differ): at most a different
    // valid proof, with its own id and its own canonical bytes. Half a body is no run at all.
    assert!(!valid(&verify_block_opts(&id(25), Parent::Genesis, &seed, 1, &b1[..b1.len() / 2], true).unwrap()));
    // A wrong frame count.
    let (frames, start, cid, body) = proof::split_blob(&b1).unwrap();
    let mut other = vec![];
    proof_header(&mut other, frames + 1, start, cid);
    other.extend(body);
    assert!(!valid(&verify_block_opts(&id(26), Parent::Genesis, &seed, 1, &other, true).unwrap()));

    // The proof id commits to the bytes: differently encoded = different block.
    assert_ne!(state_proof_id(&b1), state_proof_id(&longer));
    assert_ne!(state_proof_id(&b1), sm64pow::proof_id(&proof::encode_proof(&pr)));
    // Rebuilding (no time limit) accepts the canonical blob.
    assert!(valid(&rebuild_block_opts(&id(27), Parent::Genesis, &seed, &b1, true).unwrap()));
}

fn proof_header(out: &mut Vec<u8>, frames: u32, start: Start, cid: u8) {
    let mut v = frames as u64;
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
    out.push(start as u8);
    out.push(cid);
}

#[test]
fn bounds() {
    if !init() {
        return;
    }
    let (seed, pr) = power_on();
    let b1 = encode_proof_state(&StateCtx1, None, &seed, &params(1), &pr).unwrap();
    let (_, start, cid, body) = proof::split_blob(&b1).unwrap();
    // More frames than the time limit allows: rejected without replaying them.
    let mut huge = vec![];
    proof_header(&mut huge, 1 << 30, start, cid);
    huge.extend(body);
    assert!(!valid(&verify_block_opts(&id(31), Parent::Genesis, &seed, 1, &huge, true).unwrap()));
    // A tiny body claiming the longest allowed run: no more than `frames` frames are replayed,
    // and nothing is accepted.
    let mut long = vec![];
    proof_header(&mut long, 18_000, start, cid);
    long.extend([0xffu8; 4]);
    assert!(!valid(&verify_block_opts(&id(32), Parent::Genesis, &seed, 1, &long, true).unwrap()));
    // Empty body, zero frames, garbage.
    let mut zero = vec![];
    proof_header(&mut zero, 0, start, cid);
    invalid_with(verify_block_opts(&id(33), Parent::Genesis, &seed, 1, &zero, true).unwrap(), "empty");
    assert!(!valid(&verify_block_opts(&id(34), Parent::Genesis, &seed, 1, &[3], true).unwrap()));
}

#[test]
fn deterministic_across_runs_and_threads() {
    if !init() {
        return;
    }
    let (seed, pr) = power_on();
    let first = encode_proof_state(&StateCtx1, None, &seed, &params(1), &pr).unwrap();
    let hs: Vec<_> = (0..3)
        .map(|_| {
            let (seed, pr) = (seed, pr.clone());
            std::thread::spawn(move || encode_proof_state(&StateCtx1, None, &seed, &params(1), &pr).unwrap())
        })
        .collect();
    for h in hs {
        assert_eq!(h.join().unwrap(), first);
    }
    // Golden vector: the exact bytes of this run under this codec id. A model change
    // must be a new codec id, so this must never change.
    let digest: String = Keccak256::digest(&first).iter().map(|b| format!("{b:02x}")).collect();
    eprintln!("golden power_on blob: {} bytes keccak {digest}", first.len());
    assert_eq!(digest, GOLDEN_POWER_ON, "statectx1 output changed (new model = new codec id)");
}

const GOLDEN_POWER_ON: &str = "271e1e147843ada55eb4c70d2956df623da76012774352c1e0afa4b0b56444be";
