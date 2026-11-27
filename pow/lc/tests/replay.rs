//! The proof-of-play layer on the live chain's blocks (replayed with sm64pow's verifier, the node's own
//! code) and on forged blocks that pass every header check. Needs the game ROM: `$SM64COIN_ROM`, else
//! `../../baserom.us.z64` relative to `pow/lc`, else the test is skipped.
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use sm64coin_lc::block::{Block, Claims, RpcBlock, put_varint};
use sm64coin_lc::consensus::Sm64Consensus;
use sm64coin_lc::replay::{ReplayBudget, Sm64Replay};
use sm64coin_lc::verify::*;
use sm64pow::proof::{Proof, Start};
use sm64pow::sign::Keypair;
use sm64pow::sm64_binds::GamePad;

fn chain() -> Vec<RpcBlock> {
    include_str!("fixtures/mainnet_blocks.jsonl").lines().map(|l| RpcBlock::from_json(l).unwrap()).collect()
}

fn rom() -> bool {
    let path = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    match std::fs::read(&path) {
        Ok(b) => Sm64Replay::init(b).is_ok(),
        Err(_) => {
            eprintln!("skipping: no ROM at {path} (set SM64COIN_ROM)");
            false
        }
    }
}

struct Fixture(Vec<RpcBlock>);

#[async_trait::async_trait]
impl ChainSource for Fixture {
    fn name(&self) -> String {
        "fixture".into()
    }
    async fn tip(&self) -> Result<sm64coin_lc::proto::Tip, String> {
        let l = self.0.last().unwrap();
        Ok(sm64coin_lc::proto::Tip { height: l.claims.height, hash: l.claims.hash, cumulative_difficulty: l.claims.cumulative_difficulty })
    }
    async fn blocks(&self, from: u64, count: u32) -> Result<Vec<RpcBlock>, String> {
        Ok(self.0.iter().filter(|b| b.claims.height >= from).take(count as usize).cloned().collect())
    }
}

async fn verified(c: &[RpcBlock], cp: &Checkpoint) -> Branch {
    verify_branch(cp, &Sm64Consensus, &Fixture(c.to_vec()), &VerifyOptions::default(), &|| false).await.unwrap()
}

fn genesis_cp(c: &[RpcBlock]) -> Checkpoint {
    Checkpoint::genesis(c[0].claims.hash)
}

#[test]
fn the_ported_block_parser_agrees_with_sm64pow() {
    for b in chain().iter().skip(1) {
        let mine = Block::parse(&b.blob).unwrap();
        let theirs = sm64pow::template::split_sealed(&b.blob).unwrap();
        let facts = sm64pow::template::facts(&theirs.parts).unwrap();
        assert_eq!(mine.seed(), facts.seed, "seed at {}", mine.height);
        assert_eq!(mine.height, facts.height);
        assert_eq!(mine.header.timestamp, facts.timestamp);
        assert_eq!(mine.header.prev_id, facts.prev_id);
        assert_eq!(mine.proof, theirs.proof);
        assert_eq!(mine.sig, theirs.sig);
        assert_eq!(mine.field, theirs.field);
        assert_eq!(mine.miner_tx, theirs.parts.miner_tx);
        assert_eq!(mine.tx_hashes, theirs.parts.tx_hashes);
        assert_eq!(sm64coin_lc::block::proof_id(&mine.proof), sm64pow::proof_id(&mine.proof), "proof id at {}", mine.height);
        let ph = mine.proof_header().unwrap();
        let h = sm64pow::commit::header(&mine.proof).unwrap();
        assert_eq!((ph.frames, ph.power_on, ph.input_hash), (h.frames, h.start == Start::PowerOn, h.input_hash));
        assert_eq!(mine.nonce_bytes(), sm64pow::template::coinbase_nonce(&theirs.parts.miner_tx).unwrap().unwrap());
    }
}

#[tokio::test]
async fn the_live_chain_replays() {
    if !rom() {
        return;
    }
    let c = chain();
    let b = verified(&c, &genesis_cp(&c)).await;
    let n = b.raw.len();
    let t = Instant::now();
    let judge = Sm64Replay::new(ReplayBudget::default());
    let v = judge.judge(&b);
    let dt = t.elapsed();
    println!("replayed the current cycle ({n} blocks, {} frames) in {dt:?}: {v:?}", b.blocks.iter().map(|x| x.frames as u64).sum::<u64>());
    assert_eq!(v, JudgeVerdict::Valid { replayed: 16 });
    // a second pass is served from sm64pow's run cache
    let t = Instant::now();
    assert_eq!(judge.judge(&b), JudgeVerdict::Valid { replayed: 16 });
    println!("again, cached: {:?}", t.elapsed());
}

#[tokio::test]
async fn budgets_and_cancel_stop_the_replay_without_a_verdict_of_invalid() {
    if !rom() {
        return;
    }
    let c = chain();
    let b = verified(&c, &genesis_cp(&c)).await;
    let few = Sm64Replay::new(ReplayBudget { max_blocks: Some(3), ..Default::default() });
    assert!(matches!(few.judge(&b), JudgeVerdict::Partial { replayed: 3, .. }));
    let flag = Arc::new(AtomicBool::new(true));
    let cancelled = Sm64Replay::new(ReplayBudget { cancel: Some(flag), ..Default::default() });
    assert!(matches!(cancelled.judge(&b), JudgeVerdict::Partial { replayed: 0, reason } if reason == "cancelled"));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let mut with = Sm64Replay::new(ReplayBudget { max_blocks: Some(2), ..Default::default() });
    with.progress = Some(Arc::new(move |p| s2.lock().unwrap().push((p.done, p.total, p.height))));
    let _ = with.judge(&b);
    assert_eq!(*seen.lock().unwrap(), vec![(1, 16, 1), (2, 16, 2)]);
}

/// A block whose header, signature and weight are all valid, but whose committed run is not a run:
/// what a fake tip looks like. The header layer accepts it; only the replay refuses.
fn forged_block(prev: &RpcBlock) -> RpcBlock {
    let k = Keypair::from_entropy(&[42; 32]).unwrap();
    let nonce = sm64pow::records::template_nonce(&k.public, None, None);
    let mut mtx = vec![2];
    put_varint(&mut mtx, 1 + 72);
    mtx.extend_from_slice(&[1, 0xff]);
    put_varint(&mut mtx, 1);
    mtx.push(1);
    put_varint(&mut mtx, 1_000_000_000);
    mtx.push(0x03);
    mtx.extend_from_slice(&[0x11; 33]);
    let mut extra = vec![0x01];
    extra.extend_from_slice(&[0x22; 32]);
    extra.push(0x02);
    put_varint(&mut extra, nonce.len() as u64);
    extra.extend_from_slice(&nonce);
    put_varint(&mut mtx, extra.len() as u64);
    mtx.extend(extra);
    mtx.push(0);
    let mut head = vec![16, 16];
    put_varint(&mut head, 1_795_730_000);
    head.extend_from_slice(&prev.claims.hash);
    head.extend_from_slice(&7u32.to_le_bytes());
    let proof = sm64pow::commit::commit(&Proof { frames: 60, start: Start::PowerOn, inputs: vec![GamePad::default(); 50] });
    let assemble = |sig: &[u8; 64]| {
        let mut b = head.clone();
        b.extend_from_slice(&mtx);
        b.push(0); // no other transactions
        put_varint(&mut b, proof.len() as u64);
        b.extend_from_slice(&proof);
        b.extend_from_slice(sig);
        b.push(0);
        b
    };
    let seed = Block::parse(&assemble(&[0; 64])).unwrap().seed();
    let sig = k.sign_run(&seed, &sm64pow::commit::proof_id(&proof), 60);
    let blob = assemble(&sig);
    let p = Block::parse(&blob).unwrap();
    RpcBlock { claims: Claims { height: 1, hash: p.id(), prev_hash: p.header.prev_id, timestamp: p.header.timestamp, difficulty: 1, cumulative_difficulty: 2 }, blob }
}

#[tokio::test]
async fn a_header_valid_fake_tip_fails_its_replay() {
    if !rom() {
        return;
    }
    let c = chain();
    let cp = genesis_cp(&c);
    let fake = vec![c[0].clone(), forged_block(&c[0])];
    let b = verified(&fake, &cp).await; // passes: signature, weight, timestamps, difficulty
    assert_eq!(b.tip.height, 1);
    let v = Sm64Replay::new(ReplayBudget::default()).judge(&b);
    assert!(matches!(&v, JudgeVerdict::Invalid { height: 1, .. }), "{v:?}");
    println!("fake tip refused by the replay: {v:?}");
}

#[tokio::test]
async fn a_tampered_proof_encoding_is_caught_by_the_replay_not_the_header() {
    if !rom() {
        return;
    }
    let c = chain();
    let cp = genesis_cp(&c);
    let mut t = c.clone();
    let p = Block::parse(&t[6].blob).unwrap();
    let at = t[6].blob.windows(p.proof.len()).position(|w| w == p.proof.as_slice()).unwrap();
    let last = at + p.proof.len() / 2;
    t[6].blob[last] ^= 0x01; // (middle of the body) the id commits to the input hash, not to the encoding bytes
    assert_eq!(Block::parse(&t[6].blob).unwrap().id(), c[6].claims.hash, "the block id does not change");
    let b = verified(&t, &cp).await;
    let v = Sm64Replay::new(ReplayBudget::default()).judge(&b);
    assert!(matches!(&v, JudgeVerdict::Invalid { height: 6, .. }), "{v:?}");
}

#[tokio::test]
async fn history_between_checkpoints_is_authenticated_and_replayable() {
    if !rom() {
        return;
    }
    let c = chain();
    let lo = genesis_cp(&c);
    let hi = Checkpoint::build(&c, 8, &Sm64Consensus).unwrap();
    let blocks = backfill(&lo, &hi, &Fixture(c.clone()), &|| false).await.unwrap();
    assert_eq!(blocks.len(), 8);
    assert_eq!(Sm64Replay::new(ReplayBudget::default()).replay_history(&blocks), JudgeVerdict::Valid { replayed: 8 });
    // history that does not lead to the later checkpoint is refused
    let mut other = c.clone();
    other[4].blob[40] ^= 1;
    assert!(backfill(&lo, &hi, &Fixture(other), &|| false).await.is_err());
    let mut wrong = hi.clone();
    wrong.id[0] ^= 1;
    assert!(matches!(backfill(&lo, &wrong, &Fixture(c.clone()), &|| false).await, Err(VerifyError::CheckpointMismatch(_))));
}
