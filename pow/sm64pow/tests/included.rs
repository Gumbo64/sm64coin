//! v0.4 stage B: a block's included proofs (`sm64pow_verify_records`) and the proof pool, with the
//! included_proofs switch on and off. One test in its own process (the switches are process-wide).
//!
//! The runs of synthetic templates can't be played (each seed needs its own star), so the VALID
//! path uses a stand-in judge (the rules around the replay: root, structure, counting,
//! normalizing; the replay itself is tests/records.rs); the INVALID and BAD-WITNESS paths use the
//! real replay and need the ROM (skipped without it).

use sm64pow::chain::{Parent, RecordsVerdict};
use sm64pow::commit;
use sm64pow::features::{self, Features};
use sm64pow::ffi::{sm64pow_pool_offer, sm64pow_pool_select, sm64pow_pool_tip, sm64pow_template_nonce, sm64pow_verify_records};
use sm64pow::included::{BlockRecords, Judge, Judged, Parent1, verify_block_records};
use sm64pow::proof::{Proof, RunLength, Start};
use sm64pow::records::{self, Facts, Payout, Record};
use sm64pow::sign::{Keypair, cn_fast_hash};
use sm64pow::template::{self, TemplateParts};
use sm64pow::sm64_binds::GamePad;

const PREV: [u8; 32] = [0xab; 32];
const D: u64 = 1 << 16;

fn varint(o: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        o.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    o.push(v as u8);
}

fn coinbase(height: u64, nonce: &[u8]) -> Vec<u8> {
    let mut t = vec![2];
    varint(&mut t, height + 72);
    t.extend_from_slice(&[1, 0xff]);
    varint(&mut t, height);
    t.push(1);
    varint(&mut t, 1_000_000_000);
    t.push(0x03);
    t.extend_from_slice(&[0x11; 33]);
    let mut extra = vec![0x01];
    extra.extend_from_slice(&[0x22; 32]);
    extra.push(0x02);
    varint(&mut extra, nonce.len() as u64);
    extra.extend_from_slice(nonce);
    varint(&mut t, extra.len() as u64);
    t.extend(extra);
    t.push(0);
    t
}

fn kp(i: u8) -> Keypair {
    Keypair::from_entropy(&[i; 32]).unwrap()
}

fn payout(k: &Keypair) -> Payout {
    Payout { subaddress: false, spend: k.public, view: kp(200).public }
}

/// A record for height `h` on PREV by key `i`, a stateless run of `frames` from `start`; `lie` makes
/// the encoding not decode to the committed hash.
fn record(i: u8, h: u64, frames: u32, start: Start, lie: bool) -> Record {
    let k = kp(i);
    let pr = Proof { frames, start, inputs: vec![GamePad::new(i as u16, 0, 0); frames as usize - 10] };
    let mut blob = commit::commit_with(&RunLength, &pr);
    if lie {
        let n = blob.len();
        blob[n - 1] ^= 0xff;
    }
    let parts = TemplateParts { header: { let mut hd = vec![16, 16]; varint(&mut hd, 1_800_000_000); hd.extend_from_slice(&PREV); hd.extend_from_slice(&7u32.to_le_bytes()); hd }, miner_tx: coinbase(h, &records::template_nonce(&k.public, Some(&payout(&k)), None)), tx_hashes: vec![] };
    let seed = template::facts(&parts).unwrap().seed;
    let sig = k.sign_run(&seed, &commit::proof_id(&blob), frames);
    Record::new(&parts, blob, sig).unwrap()
}

fn sorted(mut v: Vec<Record>) -> (Vec<u8>, [u8; 32]) {
    v.sort_by_key(|r| { let f = r.facts().unwrap(); (f.run.frames, cn_fast_hash(&f.seed)) });
    let leaves: Vec<_> = v.iter().map(|r| r.leaf().unwrap()).collect();
    (records::encode(&v), records::root(&leaves))
}

struct Ok_;
impl Judge for Ok_ {
    fn start(&self, _: &Parent) -> Option<Start> {
        Some(Start::PowerOn)
    }
    fn replay_all(&self, _: Parent, _: &[Facts], _: &[&[u8]], _: usize) -> anyhow::Result<RecordsVerdict> {
        Ok(RecordsVerdict::Valid)
    }
    fn replay_one(&self, _: Parent, _: &Facts, p: &[u8]) -> anyhow::Result<Judged> {
        Ok(Judged::Valid(sm64pow::included::smallest_stateless(p)))
    }
}

/// Block h = 1 on the genesis block (id PREV), won by key 100.
fn block_h() -> (Vec<u8>, Vec<u8>) {
    let pr = Proof { frames: 2000, start: Start::PowerOn, inputs: vec![GamePad::new(1, 0, 0); 1990] };
    (sm64pow::sign::key_nonce(&kp(100).public).to_vec(), commit::commit_with(&RunLength, &pr))
}

/// The including block's nonce (a key and the root TLV) and the FFI call.
fn ffi_verify(field: &[u8], root: Option<[u8; 32]>) -> (i32, u32, Vec<u8>) {
    let (hn, hp) = block_h();
    let nonce = records::template_nonce(&kp(101).public, None, root.as_ref());
    let (mut counted, mut nlen) = (0u32, 0usize);
    let mut out = vec![0u8; 1 << 20];
    let r = sm64pow_verify_records(
        field.as_ptr(), field.len(), nonce.as_ptr(), nonce.len(), 1, PREV.as_ptr(), 1, hn.as_ptr(), hn.len(), hp.as_ptr(), hp.len(), D, 2, &mut counted, out.as_mut_ptr(), out.len(), &mut nlen,
    );
    out.truncate(nlen);
    (r, counted, out)
}

fn have_rom() -> bool {
    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    std::fs::read(&rom).map(|b| sm64pow::init(b).is_ok()).unwrap_or_else(|_| {
        eprintln!("no ROM at {rom}: skipping the real-replay cases");
        false
    })
}

#[test]
fn block_records_and_the_pool_with_the_switch_on_and_off() {
    let rom = have_rom();
    let (hn, hp) = block_h();
    let h = Parent1 { height: 1, prev_id: PREV, prev_is_genesis: true, nonce: &hn, proof: &hp, difficulty: D };
    let rs: Vec<Record> = (1..=5).map(|i| record(i, 1, 2000 + 20 * i as u32, Start::PowerOn, false)).collect();
    let (field, root) = sorted(rs.clone());
    let nonce = records::template_nonce(&kp(101).public, None, Some(&root));

    // ---- included_proofs ON (stage B) ----
    features::set(Features::STAGE_B).unwrap();
    // empty field, no root: valid at once, no ROM and no state needed
    assert_eq!(ffi_verify(&[], None).0, 1);
    assert_eq!(ffi_verify(&[], Some([0; 32])).0, 1);
    // valid records (stand-in replay): counted 0 while proof_weight is off
    match verify_block_records(&field, &nonce, &h, 2, &Ok_).unwrap() {
        BlockRecords::Valid { counted, normalized } => {
            assert_eq!(counted, 0, "no weight in stage B");
            assert!(normalized.len() <= field.len() && !normalized.is_empty());
        }
        v => panic!("{v:?}"),
    }
    features::set(Features { proof_weight: true, ..Features::STAGE_B }).unwrap();
    assert!(matches!(verify_block_records(&field, &nonce, &h, 2, &Ok_).unwrap(), BlockRecords::Valid { counted: 5, .. }));
    features::set(Features::STAGE_B).unwrap();
    // bad witness (-3): a wrong root, without needing any state
    assert_eq!(ffi_verify(&field, Some([5; 32])).0, -3);
    assert_eq!(ffi_verify(&field, None).0, -3, "records without a root TLV");
    // the parent's end state isn't cached: -2 (a block after a non-genesis parent) - here via the genesis flag off
    {
        let nonce = records::template_nonce(&kp(101).public, None, Some(&root));
        let h2 = Parent1 { prev_is_genesis: false, ..h };
        assert!(matches!(verify_block_records(&field, &nonce, &h2, 2, &sm64pow::included::Real).unwrap(), BlockRecords::NeedParent));
    }
    if rom {
        // real replay: records of height 1 on the genesis block; their runs make no progress -> invalid (0)
        assert_eq!(ffi_verify(&field, Some(root)).0, 0, "an invalid run");
        // a proof whose encoding doesn't decode to its committed hash: bad witness (-3)
        let lie = record(7, 1, 2100, Start::PowerOn, true);
        let (f, r) = sorted(vec![lie]);
        assert_eq!(ffi_verify(&f, Some(r)).0, -3);
        // a rule violation before any replay: a record on the wrong start
        let (f, r) = sorted(vec![record(8, 1, 2100, Start::Parent, false)]);
        assert_eq!(ffi_verify(&f, Some(r)).0, 0);
    }

    // ---- the pool (stand-in replay through the library path; real pool_tip/offer/select through FFI) ----
    let id = [0x77; 32];
    sm64pow_pool_tip(id.as_ptr(), 1, PREV.as_ptr(), 1, hn.as_ptr(), hn.len(), hp.as_ptr(), hp.len(), D);
    let pool = std::sync::Mutex::new(sm64pow::pool::Pool::new(records::PROPOSED));
    pool.lock().unwrap().tip(id, &h).unwrap();
    for r in &rs {
        assert_eq!(sm64pow::pool::offer_with(&pool, &enc(r), &Ok_).unwrap(), sm64pow::pool::Offer::New);
    }
    let (f, root2) = pool.lock().unwrap().select(&id).unwrap();
    assert_eq!((f.clone(), root2), sorted(rs.clone()).clone().into_pair(&f));
    // through FFI: an offer needs the real judge; a record on a height that isn't ours is ignored without replay
    assert_eq!(sm64pow_pool_offer(enc(&record(1, 9, 2000, Start::PowerOn, false)).as_ptr(), enc(&record(1, 9, 2000, Start::PowerOn, false)).len()), 0);
    assert_eq!(sm64pow_pool_offer([1u8, 2, 3].as_ptr(), 3), -1);
    let mut rootbuf = [0xffu8; 32];
    assert_eq!(sm64pow_pool_select(id.as_ptr(), std::ptr::null_mut(), 0, rootbuf.as_mut_ptr()), 0, "nothing offered through the global pool");
    assert_eq!(rootbuf, [0; 32]);

    // template_nonce
    let key = kp(3).public;
    let mn = sm64pow::sign::key_nonce(&key);
    let mut junk = mn.to_vec();
    junk.extend_from_slice(&[9, 9, 9]);
    let p = payout(&kp(4));
    let mut out = [0u8; 255];
    let n = sm64pow_template_nonce(junk.as_ptr(), junk.len(), 0, p.spend.as_ptr(), p.view.as_ptr(), root.as_ptr(), out.as_mut_ptr(), out.len());
    assert_eq!(&out[..n], records::template_nonce(&key, Some(&p), Some(&root)).as_slice(), "anything after the key is replaced");
    let n = sm64pow_template_nonce(mn.as_ptr(), mn.len(), 0, std::ptr::null(), std::ptr::null(), [0; 32].as_ptr(), out.as_mut_ptr(), out.len());
    assert_eq!(&out[..n], &mn[..], "no payout, zero root: just the key");
    assert_eq!(sm64pow_template_nonce(mn.as_ptr(), 5, 0, std::ptr::null(), std::ptr::null(), std::ptr::null(), out.as_mut_ptr(), out.len()), usize::MAX);
    assert_eq!(sm64pow_template_nonce(mn.as_ptr(), mn.len(), 0, [2; 32].as_ptr(), p.view.as_ptr(), std::ptr::null(), out.as_mut_ptr(), out.len()), usize::MAX, "not a point");

    // ---- included_proofs OFF (stage A) ----
    features::set(Features::STAGE_A).unwrap();
    assert_eq!(ffi_verify(&[], None).0, 1, "empty field, no root: still valid");
    assert_eq!(ffi_verify(&field, Some(root)).0, 0, "records refused");
    assert_eq!(ffi_verify(&[], Some(root)).0, 0, "a root refused");
    assert_eq!(ffi_verify(&field, None).0, 0, "records without a root: refused too");
    // the pool takes nothing and selects nothing
    let pool2 = std::sync::Mutex::new(sm64pow::pool::Pool::new(records::PROPOSED));
    pool2.lock().unwrap().tip(id, &h).unwrap();
    assert_eq!(sm64pow::pool::offer_with(&pool2, &enc(&rs[0]), &Ok_).unwrap(), sm64pow::pool::Offer::Ignored);
    assert_eq!(sm64pow_pool_offer(enc(&rs[0]).as_ptr(), enc(&rs[0]).len()), 0);
    assert!(pool.lock().unwrap().select(&id).is_none(), "a pool that has records selects none");
    assert_eq!(sm64pow_pool_select(id.as_ptr(), std::ptr::null_mut(), 0, rootbuf.as_mut_ptr()), 0);
    features::set(Features::ALL).unwrap();
}

fn enc(r: &Record) -> Vec<u8> {
    let mut b = vec![];
    r.encode_into(&mut b);
    b
}

trait IntoPair {
    fn into_pair(self, f: &[u8]) -> (Vec<u8>, [u8; 32]);
}
impl IntoPair for (Vec<u8>, [u8; 32]) {
    /// The pool's field has normalized proofs: same records modulo encoding, so compare leaves.
    fn into_pair(self, f: &[u8]) -> (Vec<u8>, [u8; 32]) {
        let got = records::decode(f).unwrap();
        let want = records::decode(&self.0).unwrap();
        assert_eq!(got.iter().map(|r| r.leaf().unwrap()).collect::<Vec<_>>(), want.iter().map(|r| r.leaf().unwrap()).collect::<Vec<_>>());
        (f.to_vec(), self.1)
    }
}
