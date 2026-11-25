//! v0.4 committed proofs on real runs (docs/V0.4-DESIGN.md section 1): the v0.2 mainnet
//! blocks (state codecs 4/5) and the AutoMiner's xz block, re-committed with their input hash.
//! Every encoding of a run shares its id and verdict; an encoding that doesn't decode to the
//! committed inputs is a bad encoding, never an invalid run, and never poisons the good ones.

use sm64pow::Seed;
use sm64pow::chain::{BlockVerdict, CommittedVerdict, Parent, played_wanted_block, verify_committed_block};
use sm64pow::commit::{self, input_hash};
use sm64pow::proof::{self, Proof, RunLength, Raw, Start};
use sm64pow::rules::params;

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

fn unhex(h: &str) -> Vec<u8> {
    (0..h.len() / 2).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()).collect()
}

struct Block {
    id: [u8; 32],
    prev: [u8; 32],
    ts: u64,
    difficulty: u64,
    seed: Seed,
    proof: Vec<u8>,
}

fn mainnet(n: usize) -> Vec<Block> {
    include_str!("fixtures/mainnet_v02.jsonl")
        .lines()
        .take(n)
        .map(|l| {
            let j: serde_json::Value = serde_json::from_str(l).unwrap();
            Block {
                id: unhex(j["id"].as_str().unwrap()).try_into().unwrap(),
                prev: unhex(j["prev"].as_str().unwrap()).try_into().unwrap(),
                ts: j["ts"].as_u64().unwrap(),
                difficulty: j["difficulty"].as_u64().unwrap(),
                seed: Seed::from_hex(j["seed"].as_str().unwrap()).unwrap(),
                proof: unhex(j["proof"].as_str().unwrap()),
            }
        })
        .collect()
}

fn valid(v: &CommittedVerdict) -> bool {
    matches!(v, CommittedVerdict::Run(BlockVerdict::Valid { .. }))
}

/// The committed blob of a v0.2 block (any codec) and its decoded run: the run is read with the
/// v0.3 path (a replay without the limit).
fn recommit(id: &[u8; 32], parent: Parent, b: &Block) -> (Vec<u8>, Proof) {
    let pl = played_wanted_block(id, parent, &b.seed, &b.proof).unwrap().unwrap();
    let (frames, start, _, _) = proof::split_blob(&b.proof).unwrap();
    let pr = Proof { frames, start, inputs: pl.wanted() };
    let c = commit::commit_layout(&b.proof, &input_hash(frames, start, &pr.inputs)).unwrap();
    (c, pr)
}

#[test]
fn real_state_coded_blocks_verify_committed_in_a_chain() {
    if !init() {
        return;
    }
    let blocks = mainnet(6);
    for (i, b) in blocks.iter().enumerate() {
        let parent = if i == 0 { Parent::Genesis } else { Parent::Block { id: blocks[i - 1].id, gap_seconds: b.ts - blocks[i - 1].ts } };
        assert!(i == 0 || b.prev == blocks[i - 1].id);
        let (c, pr) = recommit(&b.id, parent, b);
        assert!(sm64pow::statecodec::state_codec(proof::split_blob(&b.proof).unwrap().2).is_some(), "block {} is state-coded", i + 1);
        // The committed id is the v0.3 stateless proof_id of the same run.
        assert_eq!(commit::proof_id(&c), sm64pow::proof_id(&proof::encode_proof_with(&RunLength, &pr)));
        let v = verify_committed_block(&b.id, parent, &b.seed, b.difficulty, &c).unwrap();
        assert!(valid(&v), "block {}: {v:?}", i + 1);
        // The same run in a stateless encoding: same id, same verdict.
        let rle = commit::commit_with(&RunLength, &pr);
        assert_eq!(commit::proof_id(&rle), commit::proof_id(&c));
        assert!(valid(&verify_committed_block(&b.id, parent, &b.seed, b.difficulty, &rle).unwrap()));
    }
}

#[test]
fn bad_encodings_are_never_invalid_runs_and_never_poison_the_good_one() {
    if !init() {
        return;
    }
    let b = &mainnet(1)[0];
    let bid = [0x77; 32];
    let (good, pr) = recommit(&bid, Parent::Genesis, b);
    let (hdr, enc) = commit::split(&good).unwrap();
    // The state-coded body under another run's header: decodes, wrong hash.
    let other = commit::Header { input_hash: [9; 32], ..hdr };
    let mut truncated = good.clone();
    truncated.truncate(good.len() - 200);
    let mut flipped = good.clone();
    let n = flipped.len();
    flipped[n / 2] ^= 0x40;
    let stateless_lie = commit::join(&hdr, &proof::encode_with(&Raw, &pr.inputs[1..]));
    let bads = [("another hash", commit::join(&other, enc)), ("truncated body", truncated), ("flipped body bit", flipped), ("stateless lie", stateless_lie)];
    for (what, bad) in bads.clone() {
        let v = verify_committed_block(&bid, Parent::Genesis, &b.seed, 1, &bad).unwrap();
        assert!(matches!(v, CommittedVerdict::BadEncoding(_)), "{what}: {v:?}");
    }
    // ... and the good encodings still verify afterwards (nothing was remembered against the run).
    assert!(valid(&verify_committed_block(&bid, Parent::Genesis, &b.seed, 1, &good).unwrap()));
    assert!(valid(&verify_committed_block(&[0x78; 32], Parent::Genesis, &b.seed, 1, &commit::commit_with(&Raw, &pr)).unwrap()));
    // Once the run is known valid, a bad body under its header is still a bad encoding: a cache
    // hit never vouches for bytes nobody decoded.
    for (what, bad) in bads {
        let v = verify_committed_block(&bid, Parent::Genesis, &b.seed, 1, &bad).unwrap();
        assert!(matches!(v, CommittedVerdict::BadEncoding(_)), "{what}, after the good one: {v:?}");
    }
}

#[test]
fn an_invalid_committed_run_is_invalid_in_every_encoding() {
    if !init() {
        return;
    }
    let b = &mainnet(1)[0];
    let (_, pr) = recommit(&[0x55; 32], Parent::Genesis, b);
    // The run one frame short: the star never lands. Its hash commits the short run.
    let pl = played_wanted_block(&[0x55; 32], Parent::Genesis, &b.seed, &b.proof).unwrap().unwrap();
    let keep = pr.frames as usize - 1;
    let inputs: Vec<_> = pl.pads[..keep].iter().zip(&pl.poked[..keep]).filter(|(_, k)| !**k).map(|(p, _)| *p).collect();
    let short = Proof { frames: pr.frames - 1, start: Start::PowerOn, inputs };
    let state = sm64pow::statecodec::encode_proof_state(sm64pow::statecodec::state_codec(4).unwrap(), None, &b.seed, &params(1), &short).unwrap();
    let state = commit::commit_layout(&state, &commit::proof_hash(&short)).unwrap();
    for (what, blob) in [("state codec", state), ("run-length", commit::commit_with(&RunLength, &short))] {
        let v = verify_committed_block(&[0x56; 32], Parent::Genesis, &b.seed, 1, &blob).unwrap();
        assert!(matches!(v, CommittedVerdict::Run(BlockVerdict::Invalid(_))), "{what}: {v:?}");
    }
    // The committed length over the limit: invalid from the header alone.
    let (c, _) = recommit(&[0x57; 32], Parent::Genesis, b);
    let v = verify_committed_block(&[0x58; 32], Parent::Block { id: b.id, gap_seconds: 0 }, &b.seed, 1 << 40, &c).unwrap();
    assert!(matches!(v, CommittedVerdict::Run(BlockVerdict::Invalid(_))), "{v:?}");
}

#[test]
fn the_xz_block_recommitted() {
    if !init() {
        return;
    }
    let j: serde_json::Value = serde_json::from_str(include_str!("fixtures/power_on_star.json")).unwrap();
    let seed = Seed::from_hex(j["seed_hash"].as_str().unwrap()).unwrap();
    let xz = unhex(j["proof_hex"].as_str().unwrap());
    let pr = proof::decode_proof(&xz, 36_000).unwrap();
    let c = commit::commit_layout(&xz, &commit::proof_hash(&pr)).unwrap();
    assert_eq!(commit::proof_id(&c), sm64pow::proof_id(&xz), "v0.3 stateless ids carry over");
    assert!(valid(&verify_committed_block(&[0x31; 32], Parent::Genesis, &seed, 1, &c).unwrap()));
    let mut corrupt = c.clone();
    let n = corrupt.len();
    corrupt[n - 20] ^= 0xff;
    assert!(matches!(verify_committed_block(&[0x31; 32], Parent::Genesis, &seed, 1, &corrupt).unwrap(), CommittedVerdict::BadEncoding(_)));
}
