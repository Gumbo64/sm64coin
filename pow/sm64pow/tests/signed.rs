//! Signed proofs (v0.3, sign.rs) on real chain blocks: the v0.2 mainnet's
//! blocks 1-34 (fixtures/mainnet_v02.jsonl), each treated as if its template
//! had committed a miner key and its miner had signed the run. Every known
//! cheap variant of every block (the last byte flipped or dropped: valid
//! runs on v0.2, docs/DOS.md) must be rejected by the signature check alone,
//! with no replay. Its own process: it counts replays.

use sm64pow::Seed;
use sm64pow::chain::{BlockVerdict, Parent, replays, verify_block, verify_signed_block};
use sm64pow::sign::Keypair;
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
    height: u64,
    id: [u8; 32],
    parent: Parent,
    seed: Seed,
    difficulty: u64,
    proof: Vec<u8>,
}

fn chain() -> Vec<B> {
    let mut ts = std::collections::HashMap::new();
    let mut out = Vec::new();
    for l in include_str!("fixtures/mainnet_v02.jsonl").lines() {
        let j: serde_json::Value = serde_json::from_str(l).unwrap();
        let id: [u8; 32] = hex(j["id"].as_str().unwrap()).try_into().unwrap();
        let prev: [u8; 32] = hex(j["prev"].as_str().unwrap()).try_into().unwrap();
        let t = j["ts"].as_u64().unwrap();
        let parent = match ts.get(&prev) {
            None => Parent::Genesis,
            Some(pt) => Parent::Block { id: prev, gap_seconds: t - pt },
        };
        ts.insert(id, t);
        out.push(B {
            height: j["height"].as_u64().unwrap(),
            id,
            parent,
            seed: Seed::from_hex(j["seed"].as_str().unwrap()).unwrap(),
            difficulty: j["difficulty"].as_u64().unwrap(),
            proof: hex(j["proof"].as_str().unwrap()),
        });
    }
    out
}

/// The four cheapest edits (docs/DOS.md): last byte xor 01 / 80 / ff, and
/// the last byte dropped.
fn variants(p: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for x in [0x01u8, 0x80, 0xff] {
        let mut v = p.to_vec();
        *v.last_mut().unwrap() ^= x;
        out.push(v);
    }
    out.push(p[..p.len() - 1].to_vec());
    out
}

#[test]
fn every_mainnet_variant_is_rejected_by_the_signature_without_a_replay() {
    let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !init() {
        return;
    }
    let c = chain();
    assert_eq!(c.len(), 34);
    let (mut variants_valid_unsigned, mut variants_tried, mut blocks_with_a_variant) = (0, 0, 0);
    let mut sig_checks = std::time::Duration::ZERO;
    let attacker = Keypair::from_entropy(&[0xa7; 32]).unwrap();
    for b in &c {
        // The block's miner: a key per template, committed in the coinbase.
        let miner = Keypair::from_entropy(&[b.height as u8; 32]).unwrap();
        let nonce = miner.extra_nonce();
        let sig = sm64pow::mining::sign_proof(&miner, &b.seed.0, &b.proof).unwrap();
        let mut had_one = false;
        for (k, v) in variants(&b.proof).iter().enumerate() {
            variants_tried += 1;
            let mut vid = b.id;
            vid[0] ^= 0x40 + k as u8;
            // Under v0.3 the variant carries the miner's signature (copied), or
            // the attacker's own: both fail before any replay.
            let before = replays();
            let t = Instant::now();
            for s in [sig, sm64pow::mining::sign_proof(&attacker, &b.seed.0, v).unwrap_or(sig)] {
                let r = verify_signed_block(&vid, b.parent, &b.seed, b.difficulty, v, &nonce, &s).unwrap();
                assert!(matches!(r, BlockVerdict::Invalid(ref why) if why.contains("signature") || why.contains("bad proof")), "block {} variant {k}: {r:?}", b.height);
            }
            sig_checks += t.elapsed();
            assert_eq!(replays(), before, "block {} variant {k}: the signature check replayed", b.height);
            // Under v0.2 rules (no signature) it is often a valid block.
            vid[1] ^= 1;
            if matches!(verify_block(&vid, b.parent, &b.seed, b.difficulty, v).unwrap(), BlockVerdict::Valid { .. }) {
                variants_valid_unsigned += 1;
                had_one = true;
            }
        }
        blocks_with_a_variant += had_one as u32;
        // The real block, signed by its miner: valid, and verdicts as on v0.2.
        let unsigned = verify_block(&b.id, b.parent, &b.seed, b.difficulty, &b.proof).unwrap();
        let signed = verify_signed_block(&b.id, b.parent, &b.seed, b.difficulty, &b.proof, &nonce, &sig).unwrap();
        assert!(matches!(signed, BlockVerdict::Valid { .. }), "block {}: {signed:?}", b.height);
        assert_eq!(signed, unsigned);
        // Without the key in the coinbase, or with another key's signature: invalid.
        let other = Keypair::from_entropy(&[0xee; 32]).unwrap();
        for (n, s) in [(&[][..], sig), (&other.extra_nonce()[..], sig), (&nonce[..], other.sign_run(&b.seed.0, &sm64pow::proof_id(&b.proof), 1))] {
            assert!(matches!(verify_signed_block(&b.id, b.parent, &b.seed, b.difficulty, &b.proof, n, &s).unwrap(), BlockVerdict::Invalid(_)));
        }
    }
    eprintln!(
        "{variants_tried} variants of 34 blocks: {variants_valid_unsigned} valid without signatures ({blocks_with_a_variant} blocks have one), all rejected signed; {:.1} us per signature check",
        sig_checks.as_secs_f64() * 1e6 / (2 * variants_tried) as f64
    );
    assert!(blocks_with_a_variant >= 30, "the fixture still shows the malleability");
}

#[test]
fn a_signature_over_another_proof_or_template_fails() {
    let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !init() {
        return;
    }
    let c = chain();
    let (a, b) = (&c[0], &c[1]);
    let k = Keypair::from_entropy(&[1; 32]).unwrap();
    let sig_a = sm64pow::mining::sign_proof(&k, &a.seed.0, &a.proof).unwrap();
    let before = replays();
    // a's signature on b's proof (same key), and on a's proof under b's seed.
    let r1 = verify_signed_block(&b.id, b.parent, &b.seed, b.difficulty, &b.proof, &k.extra_nonce(), &sig_a).unwrap();
    let r2 = verify_signed_block(&a.id, a.parent, &b.seed, a.difficulty, &a.proof, &k.extra_nonce(), &sig_a).unwrap();
    assert!(matches!(r1, BlockVerdict::Invalid(_)) && matches!(r2, BlockVerdict::Invalid(_)));
    assert_eq!(replays(), before);
}

#[test]
fn one_run_per_key_on_real_blocks_whatever_the_length() {
    use sm64pow::equivocation::{Claim, policy};
    let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !init() {
        return;
    }
    let c = chain();
    let mut evidence = 0;
    for b in c.iter().take(12) {
        let miner = Keypair::from_entropy(&[0x40 + b.height as u8; 32]).unwrap();
        let nonce = miner.extra_nonce();
        // (its own template: the other tests of this process already used the real seeds)
        let mut s = b.seed;
        s.0[0] ^= 0x77;
        let sign = |p: &[u8]| sm64pow::mining::sign_proof(&miner, &s.0, p).unwrap();
        // The first run is the template's claim; the same run again is a duplicate.
        assert_eq!(sm64pow::chain::claim_signed_run(&s, &b.proof, &nonce, &sign(&b.proof)), Ok(Claim::First));
        assert_eq!(sm64pow::chain::claim_signed_run(&s, &b.proof, &nonce, &sign(&b.proof)), Ok(Claim::Same));
        // Any other run the miner signs for the template is equivocation: a valid variant of the same
        // length, and a truncated (shorter) proof, which needn't even be a valid run.
        for v in variants(&b.proof) {
            let r = sm64pow::chain::claim_signed_run(&s, &v, &nonce, &sign(&v));
            assert!(matches!(r, Ok(Claim::Equivocation(e)) if e.verify()), "block {}: {r:?}", b.height);
            evidence += 1;
        }
        let p = policy().lock().unwrap();
        assert!(p.avoid(&s.0), "block {}: the template is avoided while childless", b.height);
        // The real block still verifies: consensus never rejects for equivocation.
        drop(p);
        let real = sm64pow::mining::sign_proof(&miner, &b.seed.0, &b.proof).unwrap();
        let r = verify_signed_block(&b.id, b.parent, &b.seed, b.difficulty, &b.proof, &nonce, &real).unwrap();
        assert!(matches!(r, BlockVerdict::Valid { .. }), "block {}: {r:?}", b.height);
    }
    eprintln!("{evidence} second runs of 12 templates: all evidence, the blocks still valid");
}

#[test]
fn queue_replays_real_blocks_outside_the_lock_and_the_check_then_finds_the_result() {
    use sm64pow::queue::{Chain, Job, Verdict, global};
    let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !init() {
        return;
    }
    let c = chain();
    // A chain of 8 blocks, the first from power-on: replay each through the queue one after another
    // (each needs its parent's state), then do what the daemon does under its lock: verify.
    global().set_facts(std::sync::Arc::new(|_| Chain::Tip));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let (mut queue_replays, mut lock_replays) = (0u64, 0u64);
    for b in c.iter().take(8) {
        let miner = Keypair::from_entropy(&[0x80 + b.height as u8; 32]).unwrap();
        let sig = sm64pow::mining::sign_proof(&miner, &b.seed.0, &b.proof).unwrap();
        let before = replays();
        let parent_id = match b.parent {
            Parent::Genesis => [0; 32],
            Parent::Block { id, .. } => id,
        };
        let frames = sm64pow::proof::split_blob(&b.proof).unwrap().0;
        let job = Job { id: b.id, parent: b.parent, parent_id, seed: b.seed, difficulty: b.difficulty, blob: b.proof.clone(), frames, stamp: now, due: now, arrived: now, ctx: None };
        let t = Instant::now();
        global().submit(job).unwrap();
        let (id, v) = loop {
            if let Some(r) = global().wait_ready(std::time::Duration::from_secs(60)) {
                break r;
            }
        };
        let replay_time = t.elapsed();
        assert_eq!(id, b.id);
        assert!(matches!(v, Verdict::Valid(f) if f == frames), "block {}: {v:?}", b.height);
        assert!(global().take_cleared(&b.id));
        queue_replays += replays() - before;
        let before = replays();
        let t = Instant::now();
        let r = verify_signed_block(&b.id, b.parent, &b.seed, b.difficulty, &b.proof, &miner.extra_nonce(), &sig).unwrap();
        assert!(matches!(r, BlockVerdict::Valid { frames: f, .. } if f == frames), "block {}: {r:?}", b.height);
        lock_replays += replays() - before;
        eprintln!("block {}: {frames} frames, queue {:.0} ms, the check under the lock {:.1} ms", b.height, replay_time.as_secs_f64() * 1e3, t.elapsed().as_secs_f64() * 1e3);
    }
    // (other tests of this process may have cached some of these runs already)
    assert!(queue_replays <= 8, "at most one replay per block, in the queue");
    assert_eq!(lock_replays, 0, "the check under the blockchain lock replays nothing");
}
