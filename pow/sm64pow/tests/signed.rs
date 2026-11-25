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
