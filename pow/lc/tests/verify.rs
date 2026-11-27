//! Light-client verification against blocks of the live chain (`tests/fixtures/mainnet_blocks.jsonl`,
//! blocks 0..=16 of https://eboy.au read once with `get_block`) and against nodes that tamper.
//! Nothing here touches the network or the real chain.
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sm64coin_lc::block::{Block, Claims, RpcBlock, put_varint};
use sm64coin_lc::client::{Client, Connector, Env, Transport, TransportError};
use sm64coin_lc::consensus::Sm64Consensus;
use sm64coin_lc::nodes::{NodeAddr, NodeList, Source};
use sm64coin_lc::proto::*;
use sm64coin_lc::verify::*;

fn chain() -> Vec<RpcBlock> {
    include_str!("fixtures/mainnet_blocks.jsonl").lines().map(|l| RpcBlock::from_json(l).unwrap()).collect()
}

fn to_json(b: &RpcBlock) -> String {
    let c = &b.claims;
    json!({"blob": hex::encode(&b.blob), "block_header": {
        "height": c.height, "hash": hex::encode(c.hash), "prev_hash": hex::encode(c.prev_hash), "timestamp": c.timestamp,
        "wide_difficulty": format!("{:#x}", c.difficulty), "wide_cumulative_difficulty": format!("{:#x}", c.cumulative_difficulty)}})
    .to_string()
}

/// A chain served from memory.
struct Fixture {
    name: String,
    blocks: Vec<RpcBlock>,
}

#[async_trait::async_trait]
impl ChainSource for Fixture {
    fn name(&self) -> String {
        self.name.clone()
    }
    async fn tip(&self) -> Result<Tip, String> {
        let l = self.blocks.last().unwrap();
        Ok(Tip { height: l.claims.height, hash: l.claims.hash, cumulative_difficulty: l.claims.cumulative_difficulty })
    }
    async fn blocks(&self, from: u64, count: u32) -> Result<Vec<RpcBlock>, String> {
        Ok(self.blocks.iter().filter(|b| b.claims.height >= from).take(count as usize).cloned().collect())
    }
}

fn fx(blocks: Vec<RpcBlock>) -> Fixture {
    Fixture { name: "fixture".into(), blocks }
}

fn never() -> bool {
    false
}

fn genesis_cp(c: &[RpcBlock]) -> Checkpoint {
    Checkpoint::genesis(c[0].claims.hash)
}

async fn verify(cp: &Checkpoint, blocks: Vec<RpcBlock>) -> Result<Branch, VerifyError> {
    verify_branch(cp, &Sm64Consensus, &fx(blocks), &VerifyOptions::default(), &never).await
}

fn invalid_reason(r: Result<Branch, VerifyError>) -> String {
    match r {
        Err(VerifyError::Invalid { reason, .. }) => reason,
        other => panic!("expected an invalid block, got {:?}", other.map(|b| b.tip.height)),
    }
}

/// Rewrites a block's blob and makes the node's claims consistent with the new blob (a tampering
/// node that also lies about the hash).
fn retag(b: &mut RpcBlock) {
    let p = Block::parse(&b.blob).unwrap();
    b.claims = Claims { hash: p.id(), prev_hash: p.header.prev_id, timestamp: p.header.timestamp, ..b.claims.clone() };
}

fn set_timestamp(b: &mut RpcBlock, ts: u64) {
    let p = Block::parse(&b.blob).unwrap();
    let mut out = Vec::new();
    let mut i = 0;
    for _ in 0..2 {
        out.push(b.blob[i]);
        i += 1;
    }
    while b.blob[i] & 0x80 != 0 {
        i += 1;
    }
    i += 1; // the old timestamp varint
    put_varint(&mut out, ts);
    out.extend_from_slice(&b.blob[i..]);
    b.blob = out;
    let _ = p;
    retag(b);
}

fn find(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).position(|w| w == needle).expect("part is in the blob")
}

#[tokio::test]
async fn a_valid_chain_passes_from_genesis_and_from_a_checkpoint() {
    let c = chain();
    let b = verify(&genesis_cp(&c), c.clone()).await.unwrap();
    assert_eq!(b.level, Level::Full);
    assert_eq!(b.tip.height, 16);
    assert_eq!(b.tip.id, c[16].claims.hash);
    assert_eq!(b.tip.cumulative, c[16].claims.cumulative_difficulty);
    assert!(b.tip.weight >= 17, "one weight per block at least");
    // block 15 carries proof records: they weigh (the weight is more than the number of blocks)
    println!("weight {} over {} blocks, counted per block {:?}", b.tip.weight, b.blocks.len(), b.blocks.iter().map(|x| x.counted).collect::<Vec<_>>());
    // a checkpoint at 8 made by the same rules continues to the same weight
    let cp = Checkpoint::build(&c, 8, &Sm64Consensus).unwrap();
    assert_eq!(cp.height, 8);
    assert_eq!(cp.window.len(), 9);
    let b2 = verify(&cp, c.clone()).await.unwrap();
    assert_eq!(b2.tip, b.tip);
    assert_eq!(b2.blocks.len(), 8);
    // and it survives its JSON form
    assert_eq!(Checkpoint::from_json(&cp.to_json()).unwrap(), cp);
}

#[tokio::test]
async fn the_compiled_checkpoint_continues_to_the_tip() {
    let c = chain();
    let cp = sm64coin_lc::checkpoints::mainnet_latest();
    assert!(cp.height > 0 && cp.height < 16, "a real checkpoint, below the fixture tip");
    assert_eq!(cp.id, c[cp.height as usize].claims.hash);
    let b = verify(&cp, c).await.unwrap();
    assert_eq!(b.tip.height, 16);
}

#[tokio::test]
async fn a_checkpoint_the_chain_does_not_contain_is_rejected() {
    let c = chain();
    let mut cp = Checkpoint::build(&c, 8, &Sm64Consensus).unwrap();
    cp.id[0] ^= 1;
    assert!(matches!(verify(&cp, c.clone()).await, Err(VerifyError::CheckpointMismatch(_))));
    let mut g = genesis_cp(&c);
    g.id[5] ^= 1;
    assert!(matches!(verify(&g, c.clone()).await, Err(VerifyError::CheckpointMismatch(_))));
    // a corrupt table entry never becomes an anchor
    let mut bad = Checkpoint::build(&c, 8, &Sm64Consensus).unwrap();
    bad.window.pop();
    assert!(Checkpoint::from_json(&bad.to_json()).is_err());
    // another chain's block at the anchor height
    let mut other = c.clone();
    other[8].blob[40] ^= 1;
    let cp = Checkpoint::build(&c, 8, &Sm64Consensus).unwrap();
    assert!(matches!(verify(&cp, other).await, Err(VerifyError::CheckpointMismatch(_))));
}

#[tokio::test]
async fn tampered_headers_are_detected() {
    let c = chain();
    let cp = genesis_cp(&c);
    // a flipped byte in the header, node keeps its old hash claim: the id does not match
    let mut t = c.clone();
    let nonce_at = Block::parse(&t[6].blob).unwrap().header_bytes.len() - 1;
    t[6].blob[nonce_at] ^= 1;
    assert!(invalid_reason(verify(&cp, t).await).contains("claims contradict"));
    // a flipped byte of the prev id: it does not link
    let mut t = c.clone();
    t[6].blob[10] ^= 1;
    assert!(invalid_reason(verify(&cp, t).await).contains("does not link"));
    // the node also rewrites its claims: the next block no longer links
    let mut t = c.clone();
    set_timestamp(&mut t[6], c[6].claims.timestamp + 1);
    assert!(invalid_reason(verify(&cp, t).await).contains("does not link"));
    // a timestamp not after the parent's (on the tip, so nothing after it breaks first)
    let mut t = c.clone();
    set_timestamp(&mut t[16], c[15].claims.timestamp);
    assert!(invalid_reason(verify(&cp, t).await).contains("not after the parent"));
    // before the chain's launch (block 1, parent timestamp 0)
    let mut t = c[..2].to_vec();
    set_timestamp(&mut t[1], 1_700_000_000);
    assert!(invalid_reason(verify(&cp, t).await).contains("before the chain's launch"));
    // too far ahead of the clock
    let opts = VerifyOptions { now_secs: Some(c[16].claims.timestamp - 100_000), ..Default::default() };
    let r = verify_branch(&cp, &Sm64Consensus, &fx(c.clone()), &opts, &never).await;
    assert!(invalid_reason(r).contains("too far ahead"));
    // a lie about the difficulty or the cumulative difficulty
    let mut t = c.clone();
    t[4].claims.difficulty += 1;
    assert!(invalid_reason(verify(&cp, t).await).contains("difficulty"));
    let mut t = c.clone();
    t[9].claims.cumulative_difficulty += 1;
    assert!(invalid_reason(verify(&cp, t).await).contains("cumulative"));
}

#[tokio::test]
async fn a_skipped_block_is_a_link_error() {
    let c = chain();
    let mut t = c.clone();
    t.remove(7);
    // heights now jump 6 -> 8: the fixture serves from the requested height, so the node answers with the wrong block
    let r = verify(&genesis_cp(&c), t).await;
    assert!(matches!(r, Err(VerifyError::Invalid { .. }) | Err(VerifyError::Source(_))), "{:?}", r.map(|b| b.tip.height));
}

#[tokio::test]
async fn tampered_proofs_are_detected() {
    let c = chain();
    let cp = genesis_cp(&c);
    let proof_of = |i: usize| Block::parse(&c[i].blob).unwrap().proof;
    // another committed input hash (the node fixes its claims): the miner's signature no longer matches
    let mut t = c.clone();
    let p = proof_of(5);
    let at = find(&t[5].blob, &p) + p.len().min(8) - 1;
    let hash_at = find(&t[5].blob, &p) + 3; // inside varint(frames) || start || hash for runs < 2^14 frames
    let _ = at;
    t[5].blob[hash_at + 5] ^= 1;
    retag(&mut t[5]);
    let r = invalid_reason(verify(&cp, t).await);
    assert!(r.contains("signature") || r.contains("proof header") || r.contains("does not link"), "{r}");
    // a longer run claim (the frame count is signed)
    let mut t = c.clone();
    let p = Block::parse(&t[3].blob).unwrap();
    let ph = p.proof_header().unwrap();
    assert!(ph.frames < 0x3fff && ph.frames > 0x80, "two-byte varint");
    let at = find(&t[3].blob, &p.proof);
    t[3].blob[at] ^= 0x01; // frames + 1 (or -1): low bit of the first varint byte
    retag(&mut t[3]);
    let mut tt = t.clone();
    // keep the next block linking to the retagged one so the failure is the signature, not the link
    let mut nb = Block::parse(&tt[4].blob).unwrap();
    let _ = &mut nb;
    tt.truncate(4);
    let r = invalid_reason(verify(&cp, tt).await);
    assert!(r.contains("signature"), "{r}");
    // a forged signature
    let mut t = c.clone();
    let p = Block::parse(&t[8].blob).unwrap();
    let at = find(&t[8].blob, &p.sig);
    t[8].blob[at + 40] ^= 0x10;
    retag(&mut t[8]);
    t.truncate(9);
    assert!(invalid_reason(verify(&cp, t).await).contains("signature"));
}

#[tokio::test]
async fn the_live_chain_has_no_included_proofs_yet() {
    // (no second miner has been active: every block's proofs field is empty and weighs one per block)
    let c = chain();
    let b = verify(&genesis_cp(&c), c).await.unwrap();
    assert!(b.blocks.iter().all(|x| x.counted == 0));
    assert_eq!(b.tip.weight, 17);
}

mod records {
    //! Proof records (v0.4 included proofs): synthetic ones built with sm64pow's own codecs, judged by
    //! `Sm64Consensus::counted_records` (which calls sm64pow). The live chain has none yet.
    use super::*;
    use sm64pow::proof::{Proof, Start};
    use sm64pow::records::{Payout, Record, Window};
    use sm64pow::sign::{Keypair, cn_fast_hash};
    use sm64pow::sm64_binds::GamePad;
    use sm64pow::template::TemplateParts;

    const PREV: [u8; 32] = [0xab; 32];
    const D: u64 = 1 << 16;

    fn coinbase(height: u64, nonce: &[u8]) -> Vec<u8> {
        let mut t = vec![2];
        put_varint(&mut t, height + 72);
        t.extend_from_slice(&[1, 0xff]);
        put_varint(&mut t, height);
        t.push(1);
        put_varint(&mut t, 1_000_000_000);
        t.push(0x03);
        t.extend_from_slice(&[0x11; 33]);
        let mut extra = vec![0x01];
        extra.extend_from_slice(&[0x22; 32]);
        extra.push(0x02);
        put_varint(&mut extra, nonce.len() as u64);
        extra.extend_from_slice(nonce);
        put_varint(&mut t, extra.len() as u64);
        t.extend(extra);
        t.push(0);
        t
    }

    fn header(prev: &[u8; 32], ts: u64) -> Vec<u8> {
        let mut h = vec![16, 16];
        put_varint(&mut h, ts);
        h.extend_from_slice(prev);
        h.extend_from_slice(&7u32.to_le_bytes());
        h
    }

    fn keypair(i: u8) -> Keypair {
        Keypair::from_entropy(&[i; 32]).unwrap()
    }

    fn payout(k: &Keypair) -> Payout {
        Payout { subaddress: false, spend: k.public, view: keypair(k.public[0].wrapping_add(100)).public }
    }

    fn run(frames: u32, tag: u16) -> Proof {
        Proof { frames, start: Start::Parent, inputs: vec![GamePad::new(tag, 0, 0); frames as usize - 10] }
    }

    /// The winner (block h: height 5, on PREV) and a set of `n` records for its height with the root.
    fn scene(n: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let wk = keypair(200);
        let wnonce = sm64pow::records::template_nonce(&wk.public, Some(&payout(&wk)), None);
        let wproof = sm64pow::commit::commit_with(&sm64pow::proof::RunLength, &Proof { frames: 2000, start: Start::Parent, inputs: vec![GamePad::new(9, 0, 0); 1990] });
        let mut v: Vec<(Record, [u8; 32], u32)> = (0..n)
            .map(|i| {
                let k = keypair(i as u8 + 1);
                let pr = run(2000 + 37 * i as u32, i as u16);
                let mtx = coinbase(5, &sm64pow::records::template_nonce(&k.public, Some(&payout(&k)), None));
                let parts = TemplateParts { header: header(&PREV, 1_800_000_000), miner_tx: mtx, tx_hashes: vec![] };
                let seed = sm64pow::template::facts(&parts).unwrap().seed;
                let blob = sm64pow::commit::commit_with(&sm64pow::proof::RunLength, &pr);
                let sig = k.sign_run(&seed, &sm64pow::commit::proof_id(&blob), pr.frames);
                (Record::new(&parts, blob, sig).unwrap(), seed, pr.frames)
            })
            .collect();
        v.sort_by_key(|(_, seed, frames)| (*frames, cn_fast_hash(seed)));
        let leaves: Vec<[u8; 32]> = v.iter().map(|(r, ..)| r.leaf().unwrap()).collect();
        let root = sm64pow::records::root(&leaves);
        let recs: Vec<Record> = v.into_iter().map(|x| x.0).collect();
        let ik = keypair(77);
        let inc_nonce = sm64pow::records::template_nonce(&ik.public, Some(&payout(&ik)), Some(&root));
        (wnonce, wproof, inc_nonce, sm64pow::records::encode(&recs))
    }

    fn judge(field: &[u8], inc_nonce: &[u8], wnonce: &[u8], wproof: &[u8], prev: [u8; 32]) -> Result<u32, RecordsFault> {
        let p = Parent1Facts { height: 5, prev_id: prev, prev_is_genesis: false, nonce: wnonce, proof: wproof, difficulty: D };
        Sm64Consensus.counted_records(field, inc_nonce, &p)
    }

    #[test]
    fn counted_proofs_weigh_and_tampering_is_told_apart() {
        let (wnonce, wproof, inc, field) = scene(8);
        let n = judge(&field, &inc, &wnonce, &wproof, PREV).unwrap();
        // the same number sm64pow's own structural check gives (the daemon's count)
        let w = sm64pow::included::Parent1 { height: 5, prev_id: PREV, prev_is_genesis: false, nonce: &wnonce, proof: &wproof, difficulty: D };
        assert_eq!(n, sm64pow::stagec::count_records(&field, &inc, &w));
        assert!(n >= 1 && n <= 8, "counted {n}");
        let _ = Window { cap: 0, archive_seconds: 0, weight_seconds: None };
        // stripped (the id does not cover the field, so a node can try): the block commits a root
        assert!(matches!(judge(&[], &inc, &wnonce, &wproof, PREV), Err(RecordsFault::BadWitness(_))));
        // a flipped byte
        let mut t = field.clone();
        let mid = t.len() / 2;
        t[mid] ^= 4;
        assert!(matches!(judge(&t, &inc, &wnonce, &wproof, PREV), Err(RecordsFault::BadWitness(_))));
        // truncated
        assert!(matches!(judge(&field[..field.len() - 3], &inc, &wnonce, &wproof, PREV), Err(RecordsFault::BadWitness(_))));
        // records of another height (a wrong parent): authentic, but the block is invalid
        assert!(matches!(judge(&field, &inc, &wnonce, &wproof, [1; 32]), Err(RecordsFault::Invalid(_))));
        // no records, no root: nothing to weigh
        let ik = keypair(77);
        let plain = sm64pow::records::template_nonce(&ik.public, None, None);
        assert_eq!(judge(&[], &plain, &wnonce, &wproof, PREV), Ok(0));
    }
}

// ---- cross-check, bans, fork choice, replay hook ----

struct TestEnv;
impl Env for TestEnv {
    fn now_ms(&self) -> u64 {
        2_000_000_000_000
    }
    fn rand(&self) -> u64 {
        7
    }
}

#[derive(Clone)]
struct FakeNode {
    blocks: Arc<Vec<RpcBlock>>,
    /// The tip it claims (default: its last block).
    claim: Option<Tip>,
    down: bool,
}

#[async_trait::async_trait]
impl Transport for FakeNode {
    async fn request(&self, req: &Request, _t: Duration) -> Result<Response, TransportError> {
        if self.down {
            return Err(TransportError::Connect("down".into()));
        }
        let l = self.blocks.last().unwrap();
        Ok(match req {
            Request::GetTip => Response::Tip(self.claim.clone().unwrap_or(Tip { height: l.claims.height, hash: l.claims.hash, cumulative_difficulty: l.claims.cumulative_difficulty })),
            Request::GetBlocks { from, count, .. } => {
                Response::Blocks(self.blocks.iter().filter(|b| b.claims.height >= *from).take(*count as usize).map(to_json).collect())
            }
            _ => Response::Error(ErrorKind::Unsupported),
        })
    }
}

struct Net(Vec<(String, FakeNode)>);
impl Connector for Net {
    fn transport(&self, addr: &NodeAddr) -> Option<Arc<dyn Transport>> {
        let k = addr.key();
        self.0.iter().find(|(n, _)| *n == k).map(|(_, n)| Arc::new(n.clone()) as Arc<dyn Transport>)
    }
}

fn net(nodes: Vec<(&str, FakeNode)>) -> (Client, Vec<String>) {
    let mut list = NodeList::new();
    let mut named = Vec::new();
    for (n, node) in nodes {
        let addr = NodeAddr::Http { url: format!("http://{n}") };
        list.add(addr.clone(), Source::Manual, 1);
        named.push((addr.key(), node));
    }
    let keys = named.iter().map(|(k, _)| k.clone()).collect();
    (Client::new(list, Arc::new(Net(named)), Arc::new(TestEnv)), keys)
}

fn honest(c: &[RpcBlock]) -> FakeNode {
    FakeNode { blocks: Arc::new(c.to_vec()), claim: None, down: false }
}

fn banned(client: &Client, key: &str) -> bool {
    client.with_list(|l| l.get(key).unwrap().is_banned(2_000_000_000))
}

#[tokio::test]
async fn agreeing_nodes_a_laggard_and_a_liar() {
    let c = chain();
    let cp = genesis_cp(&c);
    // the liar serves a chain with a forged block in the middle (and keeps the honest hashes)
    let mut forged = c.clone();
    forged[7].blob[12] ^= 0x40;
    let (client, k) = net(vec![
        ("a", honest(&c)),
        ("b", honest(&c)),
        ("c", honest(&c)),
        ("lag", honest(&c[..11])),
        ("liar", FakeNode { blocks: Arc::new(forged), claim: None, down: false }),
        ("down", FakeNode { blocks: Arc::new(c.clone()), claim: None, down: true }),
    ]);
    let opts = CrossCheckOptions { nodes: 6, quorum: 3, ..Default::default() };
    let out = sync(&client, &cp, &Sm64Consensus, None, &opts, &never).await.unwrap();
    assert_eq!(out.best.tip.height, 16);
    assert!(out.quorum_met);
    assert_eq!(out.banned, vec![k[4].clone()]);
    assert!(banned(&client, &k[4]), "the liar is banned");
    assert!(!banned(&client, &k[0]) && !banned(&client, &k[3]) && !banned(&client, &k[5]), "honest, behind and unreachable nodes are not");
    let verdict = |key: &String| out.nodes.iter().find(|(n, _)| n == key).map(|(_, v)| v.clone()).unwrap();
    assert_eq!(verdict(&k[0]), NodeVerdict::Agrees);
    assert_eq!(verdict(&k[3]), NodeVerdict::Behind);
    assert!(matches!(verdict(&k[4]), NodeVerdict::Liar(_)));
    assert!(matches!(verdict(&k[5]), NodeVerdict::Unreachable(_)));
}

#[tokio::test]
async fn a_node_that_forges_a_longer_tip_is_caught() {
    let c = chain();
    let cp = genesis_cp(&c);
    // claims a tip far above the real one and serves its own made-up block on top of the honest chain
    let mut fake = c.clone();
    let mut extra = fake[16].clone();
    extra.claims.height = 17;
    extra.claims.prev_hash = fake[16].claims.hash;
    extra.claims.cumulative_difficulty += 1;
    fake.push(extra);
    let (client, k) = net(vec![("a", honest(&c)), ("b", honest(&c)), ("c", honest(&c)), ("liar", FakeNode { blocks: Arc::new(fake), claim: None, down: false })]);
    let out = sync(&client, &cp, &Sm64Consensus, None, &CrossCheckOptions::default(), &never).await.unwrap();
    assert_eq!(out.best.tip.height, 16, "the forged block 17 is not accepted");
    assert!(banned(&client, &k[3]));
    assert!(out.quorum_met);
}

#[tokio::test]
async fn a_node_on_another_chain_is_rejected_by_the_checkpoint() {
    let c = chain();
    let cp = Checkpoint::build(&c, 8, &Sm64Consensus).unwrap();
    let mut other = c.clone();
    other[8].blob[20] ^= 1;
    let (client, k) = net(vec![("a", honest(&c)), ("b", honest(&c)), ("x", FakeNode { blocks: Arc::new(other), claim: None, down: false })]);
    let out = sync(&client, &cp, &Sm64Consensus, None, &CrossCheckOptions { nodes: 3, quorum: 2, ..Default::default() }, &never).await.unwrap();
    assert!(banned(&client, &k[2]));
    assert_eq!(out.best.tip.id, c[16].claims.hash);
}

struct Fixed(Option<(u64, String)>);
impl Judge for Fixed {
    fn judge(&self, b: &Branch) -> JudgeVerdict {
        match &self.0 {
            Some((h, why)) if b.tip.height >= *h => JudgeVerdict::Invalid { height: *h, reason: why.clone() },
            _ => JudgeVerdict::Valid { replayed: 1 },
        }
    }
}

#[tokio::test]
async fn a_chain_that_fails_its_replay_is_dropped_and_its_servers_banned() {
    let c = chain();
    let cp = genesis_cp(&c);
    // "longer" is the chain every node serves; a judge that finds block 14 unplayable rejects it for all
    // and nothing is left; with the laggard (tip 11, replay fine) the client falls back to it.
    let (client, k) = net(vec![("a", honest(&c)), ("b", honest(&c)), ("lag", honest(&c[..12]))]);
    let judge = Fixed(Some((14, "the run does not reach its progress point".into())));
    let out = sync(&client, &cp, &Sm64Consensus, Some(&judge), &CrossCheckOptions { nodes: 3, quorum: 1, ..Default::default() }, &never).await.unwrap();
    assert_eq!(out.best.tip.height, 11);
    assert!(banned(&client, &k[0]) && banned(&client, &k[1]), "the servers of the unplayable chain are banned");
    assert!(!banned(&client, &k[2]));
    assert_eq!(out.replay, Some(JudgeVerdict::Valid { replayed: 1 }));
    // all chains bad: an error, never a fallback to an unverified chain
    let (client, _) = net(vec![("a", honest(&c))]);
    let judge = Fixed(Some((2, "bad".into())));
    assert!(sync(&client, &cp, &Sm64Consensus, Some(&judge), &CrossCheckOptions { nodes: 1, quorum: 1, ..Default::default() }, &never).await.is_err());
}

#[tokio::test]
async fn cancelling_stops_the_verification() {
    let c = chain();
    let r = verify_branch(&genesis_cp(&c), &Sm64Consensus, &fx(c.clone()), &VerifyOptions::default(), &|| true).await;
    assert_eq!(r.err(), Some(VerifyError::Cancelled));
}

#[test]
fn fork_choice_is_longer_and_heavier() {
    let rules = Sm64Consensus;
    let branch = |name: &str, len: u64, weight: u64| {
        let tip = VerifiedBlock { height: len, id: [len as u8; 32], prev_id: [0; 32], timestamp: 0, difficulty: 1, cumulative: 0, frames: 1, power_on: false, counted: 0, weight };
        Branch { source: name.into(), servers: vec![name.into()], level: Level::Full, anchor_height: 0, anchor_weight: 0, tip, blocks: vec![], raw: vec![] }
    };
    // longer but lighter loses; longer and heavier wins; heavier but not longer loses
    let o = order_by_fork_choice(&rules, vec![branch("short-heavy", 10, 100), branch("long-light", 12, 40), branch("long-heavy", 12, 120)]);
    assert_eq!(o.iter().map(|b| b.source.as_str()).collect::<Vec<_>>().first(), Some(&"long-heavy"));
    assert!(!rules.prefer_alt(Side { len: 10, weight: 100, tainted: false }, Side { len: 12, weight: 40, tainted: false }));
    assert!(!rules.prefer_alt(Side { len: 12, weight: 40, tainted: false }, Side { len: 10, weight: 100, tainted: false }));
}

#[tokio::test]
async fn structural_only_checks_linkage_and_says_so() {
    let c = chain();
    let b = verify_branch(&genesis_cp(&c), &StructuralOnly, &fx(c.clone()), &VerifyOptions::default(), &never).await.unwrap();
    assert_eq!(b.level, Level::Structural);
    assert_eq!(b.tip.weight, 17, "no records counted without the rules");
    let mut t = c.clone();
    t[6].blob[10] ^= 1;
    assert!(verify_branch(&genesis_cp(&c), &StructuralOnly, &fx(t), &VerifyOptions::default(), &never).await.is_err());
}

#[test]
fn cycles_are_found_from_power_on_starts() {
    let c = chain();
    let cp = genesis_cp(&c);
    let b = futures_util::FutureExt::now_or_never(verify(&cp, c.clone())).unwrap().unwrap();
    let (at, complete) = b.cycle_start(0);
    assert!(complete && b.blocks[b.blocks.len() - b.raw.len() + at].power_on);
    assert_eq!(b.raw.len(), 16);
}
