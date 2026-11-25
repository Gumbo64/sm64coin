//! Denial of service (docs/DOS.md): what an attacker can make a node replay,
//! on real chain blocks (fixtures/chain_head.jsonl: mainnet blocks 1-4).
//! Its own process: it counts replays.

use sm64pow::Seed;
use sm64pow::admit::{AltBlock, Context, Decision, Source};
use sm64pow::chain::{BatchBlock, BlockVerdict, Parent, preverify, rebuild_block, replays, verify_block};
use std::time::Instant;

/// The tests count replays (a process-wide counter): one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

struct B {
    id: [u8; 32],
    parent: Parent,
    seed: Seed,
    proof: Vec<u8>,
}

fn chain() -> Vec<B> {
    let mut ts = std::collections::HashMap::new();
    let mut out = Vec::new();
    for l in include_str!("fixtures/chain_head.jsonl").lines() {
        let j: serde_json::Value = serde_json::from_str(l).unwrap();
        let id: [u8; 32] = hex(j["id"].as_str().unwrap()).try_into().unwrap();
        let prev: [u8; 32] = hex(j["prev"].as_str().unwrap()).try_into().unwrap();
        let t = j["ts"].as_u64().unwrap();
        let parent = match ts.get(&prev) {
            None => Parent::Genesis,
            Some(pt) => Parent::Block { id: prev, gap_seconds: t - pt },
        };
        ts.insert(id, t);
        out.push(B { id, parent, seed: Seed::from_hex(j["seed"].as_str().unwrap()).unwrap(), proof: hex(j["proof"].as_str().unwrap()) });
    }
    out
}

/// A proof blob claiming `frames` frames from power-on, with the body of `like`.
fn claim(like: &[u8], frames: u64) -> Vec<u8> {
    let (_, _, cid, body) = sm64pow::proof::split_blob(like).unwrap();
    let mut out = Vec::new();
    let mut v = frames;
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
    out.push(1); // power-on
    out.push(cid);
    out.extend_from_slice(body);
    out
}

fn verify_all(c: &[B]) {
    for b in c {
        assert!(matches!(verify_block(&b.id, b.parent, &b.seed, 1, &b.proof).unwrap(), BlockVerdict::Valid { .. }));
    }
}

/// The free attack: the last byte of a published proof, flipped or dropped,
/// is often another valid run (the last few inputs differ: a run-level
/// malleability, not an encoding one: every variant is canonical). Same seed,
/// same length, new block id. The policy must recognise them without a replay.
#[test]
fn malleated_variants_are_valid_and_the_policy_skips_them() {
    let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !init() {
        return;
    }
    let c = chain();
    verify_all(&c);
    let b = &c[1]; // block 2: its "drop last byte" variant is valid
    let v = b.proof[..b.proof.len() - 1].to_vec();
    let mut vid = b.id;
    vid[0] ^= 1;
    assert!(matches!(verify_block(&vid, b.parent, &b.seed, 1, &v).unwrap(), BlockVerdict::Valid { .. }), "a third party's variant is a valid block");
    assert_ne!(sm64pow::proof_id(&v), sm64pow::proof_id(&b.proof), "with its own block id");
    // As an alternative block at our tip's height (we have block 2 already): not replayed.
    let Parent::Block { id: parent, .. } = b.parent else { panic!() };
    let alt = AltBlock { id: vid, parent, seed: b.seed, frames: sm64pow::proof::split_blob(&v).unwrap().0, height: 2, top: 4, main_seed: None };
    let relay = Context { host: "203.0.113.9".into(), source: Source::Relay };
    let _g = sm64pow::admit::enter(Some(relay));
    assert!(matches!(sm64pow::admit::admit_alt(&alt), Decision::Skip(_)));
    // Unless a longer chain comes with it.
    let _s = sm64pow::admit::enter(Some(Context { host: "203.0.113.9".into(), source: Source::Sync { top: 5 } }));
    assert_eq!(sm64pow::admit::admit_alt(&alt), Decision::Replay);
}

/// The parallel sync pre-pass replays a batch before any rule looks at it.
/// Fake blocks claiming the codecs' cap (2^20 frames) used to be replayed to
/// the end (~30 s each here); now nothing past the longest limit is replayed,
/// and nothing after the first invalid block.
#[test]
fn a_batch_of_fake_blocks_costs_the_prepass_almost_nothing() {
    let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !init() {
        return;
    }
    let c = chain();
    let like = &c[0].proof;
    let fake = |k: u8, frames: u64| {
        let mut id = [0xf0; 32];
        id[0] = k;
        let mut seed = c[0].seed;
        seed.0[0] ^= k;
        BatchBlock { id, parent: Parent::Block { id: [0xaa; 32], gap_seconds: 0 }, seed, blob: claim(like, frames) }
    };
    // 1. Over the longest limit: not replayed at all.
    let batch: Vec<BatchBlock> = (1..=16).map(|k| fake(k, 1 << 20)).collect();
    let (r0, t) = (replays(), Instant::now());
    preverify(&batch, 4);
    assert_eq!(replays(), r0, "no claim over the longest limit is replayed");
    assert!(t.elapsed().as_secs_f64() < 1.0);
    // 2. Within the limit but invalid (another seed): about one replay per
    // thread, not one per block.
    let limit = sm64pow::rules().params(1).max_frames as u64;
    let batch: Vec<BatchBlock> = (17..=48).map(|k| fake(k, limit)).collect();
    let r0 = replays();
    preverify(&batch, 2);
    let n = replays() - r0;
    assert!(n <= 4, "{n} of 32 fake blocks replayed");
    // 3. Sent again: known invalid, no replay at all.
    let r0 = replays();
    preverify(&batch, 2);
    assert_eq!(replays(), r0);
}

/// An invalid block sent again (or re-stamped) is not replayed again.
#[test]
fn invalid_runs_are_remembered() {
    let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !init() {
        return;
    }
    let c = chain();
    verify_all(&c);
    let b = &c[0];
    let mut seed = b.seed;
    seed.0[5] ^= 0x40; // another nonce: the run no longer works
    let v = verify_block(&[0x11; 32], b.parent, &seed, 1, &b.proof).unwrap();
    assert!(matches!(v, BlockVerdict::Invalid(_)), "{v:?}");
    let r0 = replays();
    // The same block again, and a re-stamped copy (new id, other gap).
    assert!(matches!(verify_block(&[0x11; 32], b.parent, &seed, 1, &b.proof).unwrap(), BlockVerdict::Invalid(_)));
    assert!(matches!(verify_block(&[0x12; 32], b.parent, &seed, 1, &b.proof).unwrap(), BlockVerdict::Invalid(_)));
    assert_eq!(replays(), r0);
    // A rule that depends on the timestamp is not a property of the run: a
    // block too long for its stamp, then re-stamped later, is replayed and valid.
    let d = c[1].clone_parent_gap(1);
    let limit_short = sm64pow::rules().max_frames(1 << 20, 1);
    let frames = sm64pow::proof::split_blob(&c[1].proof).unwrap().0;
    assert!(frames > limit_short);
    assert!(matches!(verify_block(&[0x21; 32], d, &c[1].seed, 1 << 20, &c[1].proof).unwrap(), BlockVerdict::Invalid(_)));
    assert!(matches!(verify_block(&[0x22; 32], c[1].parent, &c[1].seed, 1, &c[1].proof).unwrap(), BlockVerdict::Valid { .. }));
}

trait Gap {
    fn clone_parent_gap(&self, gap: u64) -> Parent;
}
impl Gap for B {
    fn clone_parent_gap(&self, gap: u64) -> Parent {
        match self.parent {
            Parent::Block { id, .. } => Parent::Block { id, gap_seconds: gap },
            p => p,
        }
    }
}

/// Replays an RPC call causes are budgeted: once the budget is spent, an RPC
/// replay fails ("busy") while cached answers and consensus replays go on.
#[test]
fn rpc_replays_are_budgeted() {
    let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !init() {
        return;
    }
    let c = chain();
    verify_all(&c);
    // Spend the budget.
    sm64pow::admit::policy().lock().unwrap().rpc_replay_charge(1000.0, sm64pow::admit::now());
    {
        let _rpc = sm64pow::admit::RpcScope::enter();
        // Already verified: answered from the cache.
        let b = &c[2];
        assert!(matches!(verify_block(&b.id, b.parent, &b.seed, 1, &b.proof).unwrap(), BlockVerdict::Valid { .. }));
        // A run that needs a replay (block 3's pads, never asked for): busy.
        let e = sm64pow::chain::played_block(&b.id, b.parent, &b.seed, &b.proof);
        assert!(e.is_err() && e.unwrap_err().to_string().starts_with("busy"));
    }
    // Outside an RPC call: unaffected.
    let b = &c[3];
    assert!(sm64pow::chain::played_block(&b.id, b.parent, &b.seed, &b.proof).unwrap().is_ok());
    let _ = rebuild_block(&b.id, b.parent, &b.seed, &b.proof).unwrap();
}
