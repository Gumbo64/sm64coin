//! v0.4 stage C and wiring (records.rs proof outputs, stagec.rs, ffi): each switch on and off. One
//! test in its own process (the switches are process-wide). The records-pre-pass replay needs the
//! ROM (skipped without it); synthetic templates can't make valid runs, so the replays it makes
//! are of invalid runs, which is what proves the pre-pass ran them and the sequential check found
//! the cached verdict.

use sm64pow::codecs::Mask;
use sm64pow::commit;
use sm64pow::features::{self, Features};
use sm64pow::ffi::*;
use sm64pow::proof::{Proof, RunLength, Start};
use sm64pow::records::{self, PROPOSED_REWARD, Payout, Record};
use sm64pow::sign::{Keypair, cn_fast_hash};
use sm64pow::sm64_binds::GamePad;
use sm64pow::template::{self, TemplateParts};

const PREV: [u8; 32] = [0xab; 32];
const D: u64 = 1 << 16;
const BASE: u64 = 1 << 40;

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

fn header(prev: &[u8; 32]) -> Vec<u8> {
    let mut hd = vec![16, 16];
    varint(&mut hd, 1_800_000_000);
    hd.extend_from_slice(prev);
    hd.extend_from_slice(&7u32.to_le_bytes());
    hd
}

fn record_on(prev: &[u8; 32], i: u8, h: u64, frames: u32) -> Record {
    let k = kp(i);
    let payout = Payout { subaddress: i % 2 == 0, spend: kp(i + 50).public, view: kp(200).public };
    let pr = Proof { frames, start: Start::Parent, inputs: vec![GamePad::new(i as u16, 0, 0); frames as usize - 10] };
    let blob = commit::commit_with(&RunLength, &pr);
    let parts = TemplateParts { header: header(prev), miner_tx: coinbase(h, &records::template_nonce(&k.public, Some(&payout), None)), tx_hashes: vec![] };
    let seed = template::facts(&parts).unwrap().seed;
    let sig = k.sign_run(&seed, &commit::proof_id(&blob), frames);
    Record::new(&parts, blob, sig).unwrap()
}

fn sorted(mut v: Vec<Record>) -> (Vec<u8>, [u8; 32]) {
    v.sort_by_key(|r| {
        let f = r.facts().unwrap();
        (f.run.frames, cn_fast_hash(&f.seed))
    });
    let leaves: Vec<_> = v.iter().map(|r| r.leaf().unwrap()).collect();
    (records::encode(&v), records::root(&leaves))
}

fn outputs(field: &[u8], first: u32) -> Option<Vec<u8>> {
    let n = sm64pow_proof_outputs(field.as_ptr(), field.len(), BASE, first, std::ptr::null_mut(), 0);
    if n == usize::MAX {
        return None;
    }
    let mut b = vec![0u8; n];
    assert_eq!(sm64pow_proof_outputs(field.as_ptr(), field.len(), BASE, first, b.as_mut_ptr(), n), n);
    Some(b)
}

fn miner_tx_ok(field: &[u8], outs: &[Vec<u8>], add: &[[u8; 32]]) -> i32 {
    let o: Vec<u8> = outs.concat();
    let a: Vec<u8> = add.concat();
    sm64pow_check_miner_tx(field.as_ptr(), field.len(), BASE, o.as_ptr(), outs.len(), a.as_ptr(), add.len())
}

fn have_rom() -> bool {
    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    std::fs::read(&rom).map(|b| sm64pow::init(b).is_ok()).unwrap_or_else(|_| {
        eprintln!("no ROM at {rom}: skipping the real-replay cases");
        false
    })
}

fn fc(main_len: u64, main: &[([u8; 32], u32)], alt_len: u64, alt: &[([u8; 32], u32)]) -> i32 {
    let (ms, mc): (Vec<u8>, Vec<u32>) = (main.iter().flat_map(|x| x.0).collect(), main.iter().map(|x| x.1).collect());
    let (as_, ac): (Vec<u8>, Vec<u32>) = (alt.iter().flat_map(|x| x.0).collect(), alt.iter().map(|x| x.1).collect());
    sm64pow_fork_choice_weighted(main_len, ms.as_ptr(), mc.as_ptr(), main.len(), alt_len, as_.as_ptr(), ac.as_ptr(), alt.len())
}

#[test]
fn stage_c_each_switch_on_and_off() {
    let rom = have_rom();
    let rs: Vec<Record> = (1..=5).map(|i| record_on(&PREV, i, 1, 2000 + 20 * i as u32)).collect();
    let (field, root) = sorted(rs.clone());
    let n = rs.len();
    let hn = sm64pow::sign::key_nonce(&kp(100).public).to_vec();
    let hproof = commit::commit_with(&RunLength, &Proof { frames: 2000, start: Start::PowerOn, inputs: vec![GamePad::new(1, 0, 0); 1990] });
    let nonce = records::template_nonce(&kp(101).public, None, Some(&root));
    let count = |field: &[u8]| sm64pow_count_records(field.as_ptr(), field.len(), nonce.as_ptr(), nonce.len(), 1, PREV.as_ptr(), 1, hn.as_ptr(), hn.len(), hproof.as_ptr(), hproof.len(), D);
    let amount = records::proof_reward(BASE, n, &PROPOSED_REWARD);

    // ---- proof_rewards ON (everything on) ----
    features::set(Features::ALL).unwrap();
    assert!(amount > 0);
    let o = outputs(&field, 1).unwrap();
    assert_eq!(o.len(), 73 * n);
    let recs = records::decode(&field).unwrap();
    let mut outs = vec![vec![0u8; 41]]; // the miner's own output
    let mut add = vec![[0u8; 32]];
    for (i, r) in recs.iter().enumerate() {
        let f = r.facts().unwrap();
        let want = sm64pow::payout::proof_output(&f.leaf(&r.sig), &f.payout, 1 + i as u64).unwrap();
        let c = &o[73 * i..73 * i + 73];
        assert_eq!((&c[..32], c[32], &c[33..65]), (&want.key[..], want.view_tag, &want.tx_pubkey[..]));
        assert_eq!(u64::from_le_bytes(c[65..].try_into().unwrap()), amount);
        let mut out = c[..33].to_vec();
        out.extend_from_slice(&c[65..]);
        outs.push(out);
        add.push(want.tx_pubkey);
    }
    // the output index is part of the key
    assert_ne!(outputs(&field, 2).unwrap()[..32], o[..32]);
    assert_eq!(miner_tx_ok(&field, &outs, &add), 1);
    // untagged outputs (tag 0) are fine; a wrong tag is not (unless the derived tag is 0)
    let mut un = outs.clone();
    un[1][32] = 0;
    assert_eq!(miner_tx_ok(&field, &un, &add), 1);
    // each way to be wrong
    let mut bad = outs.clone();
    bad[2][0] ^= 1;
    assert_eq!(miner_tx_ok(&field, &bad, &add), 0, "wrong output key");
    let mut bad = outs.clone();
    bad[3][33] ^= 1;
    assert_eq!(miner_tx_ok(&field, &bad, &add), 0, "wrong amount");
    let mut bad_add = add.clone();
    bad_add[1][0] ^= 1;
    assert_eq!(miner_tx_ok(&field, &outs, &bad_add), 0, "wrong additional key");
    assert_eq!(miner_tx_ok(&field, &outs, &add[..n]), 0, "missing additional key");
    assert_eq!(miner_tx_ok(&field, &outs[..n], &add), 0, "a proof is not paid");
    let mut more = outs.clone();
    more.push(vec![0; 41]);
    assert_eq!(miner_tx_ok(&field, &more, &add), 0, "an extra output");
    assert_eq!(miner_tx_ok(&field, &outs[..1], &add[..1]), 0, "proofs must be paid");
    let mut swapped = outs.clone();
    swapped.swap(1, 2);
    assert_eq!(miner_tx_ok(&field, &swapped, &add), 0, "record order");
    // no records: exactly the miner output
    assert_eq!(miner_tx_ok(&[], &outs[..1], &add[..1]), 1);
    assert_eq!(miner_tx_ok(&[], &outs[..1], &[]), 1);
    assert_eq!(miner_tx_ok(&[], &outs, &add), 0, "a proof output with no proofs");
    assert_eq!(outputs(&[], 1), Some(vec![]));
    assert_eq!(outputs(&[1, 2, 3], 1), None, "unparsable");
    assert_eq!(miner_tx_ok(&[1, 2, 3], &outs[..1], &add[..1]), 0);

    // ---- proof_rewards OFF (weight on) ----
    features::set(Features { proof_rewards: false, ..Features::ALL }).unwrap();
    assert_eq!(outputs(&field, 1), Some(vec![]));
    assert_eq!(miner_tx_ok(&field, &outs[..1], &add[..1]), 1, "no proof outputs allowed, records or not");
    assert_eq!(miner_tx_ok(&field, &outs, &add), 0);

    // ---- count_records: weight on / off ----
    assert_eq!(count(&field), n as u32);
    assert_eq!(count(&field[..field.len() - 1]), 0, "truncated");
    let nonce_bad = records::template_nonce(&kp(101).public, None, Some(&[5; 32]));
    assert_eq!(sm64pow_count_records(field.as_ptr(), field.len(), nonce_bad.as_ptr(), nonce_bad.len(), 1, PREV.as_ptr(), 1, hn.as_ptr(), hn.len(), hproof.as_ptr(), hproof.len(), D), 0, "wrong root");
    assert_eq!(count(&[]), 0);
    features::set(Features::STAGE_B).unwrap();
    assert_eq!(count(&field), 0, "proof_weight off");
    features::set(Features { proof_weight: true, ..Features::STAGE_B }).unwrap();
    assert_eq!(count(&field), n as u32);

    // ---- fork choice ----
    let b = |i: u8, c: u32| ([i; 32], c);
    let (m, a_heavy, a_light) = ([b(1, 2), b(2, 2)], [b(3, 5), b(4, 5), b(5, 5)], [b(3, 0), b(4, 0), b(5, 0)]);
    features::set(Features { proof_weight: true, ..Features::STAGE_B }).unwrap();
    assert_eq!(fc(2, &m, 3, &a_heavy), 1, "longer and heavier");
    assert_eq!(fc(2, &m, 3, &a_light), 0, "longer but lighter: keep");
    assert_eq!(fc(2, &m, 2, &a_heavy[..2]), 0, "heavier but not longer: keep");
    assert_eq!(fc(2, &m, 3, &[]), 0, "blocks beyond the entries weigh 1 each: 3 against 6");
    features::set(Features::STAGE_B).unwrap();
    assert_eq!(fc(2, &m, 3, &a_light), 1, "weight off: v0.3's longest chain");
    assert_eq!(fc(2, &m, 2, &a_heavy[..2]), 0);
    let seeds: Vec<u8> = a_light.iter().flat_map(|x| x.0).chain([]).collect();
    let mseeds: Vec<u8> = m.iter().flat_map(|x| x.0).collect();
    assert_eq!(sm64pow_fork_choice(2, mseeds.as_ptr(), 2, 3, seeds.as_ptr(), 3), fc(2, &m, 3, &a_light), "exactly sm64pow_fork_choice");
    features::set(Features::NONE).unwrap();
    assert_eq!(fc(2, &m, 3, &a_light), 1);
    assert_eq!(count(&field), 0);
    assert_eq!(outputs(&field, 1), Some(vec![]));
    features::set(Features::ALL).unwrap();

    // ---- records_for_peer ----
    let mut p = Vec::new();
    let mut call = |mask: *const u8| {
        let n = sm64pow_records_for_peer(field.as_ptr(), field.len(), mask, std::ptr::null_mut(), 0);
        if n == 0 || n == usize::MAX {
            return n;
        }
        let mut b = vec![0u8; n];
        sm64pow_records_for_peer(field.as_ptr(), field.len(), mask, b.as_mut_ptr(), n);
        p = b;
        n
    };
    assert_eq!(call(std::ptr::null()), 0, "baseline decodes run-length: no change");
    let mut only_raw = Mask::EMPTY;
    only_raw.insert(0);
    let n2 = call(only_raw.0.as_ptr());
    assert!(n2 != 0 && n2 != usize::MAX);
    let got = records::decode(&p).unwrap();
    assert_eq!(got.iter().map(|r| r.leaf().unwrap()).collect::<Vec<_>>(), recs.iter().map(|r| r.leaf().unwrap()).collect::<Vec<_>>(), "same leaves, same root");
    assert!(got.iter().all(|r| sm64pow::codecs::codec_of(&r.proof) == Some(0)));
    // a state-coded record: needs its inputs; refused without, transcoded once they are known
    let pr = Proof { frames: 300, start: Start::Parent, inputs: vec![GamePad::new(9, 0, 0); 290] };
    let state = commit::join(&commit::header_of(&pr), &[5, 1, 2, 3]);
    let mut sr = recs[0].clone();
    sr.proof = state;
    let sfield = records::encode(&[sr]);
    let sn = sm64pow_records_for_peer(sfield.as_ptr(), sfield.len(), std::ptr::null(), std::ptr::null_mut(), 0);
    assert_eq!(sn, usize::MAX, "inputs not cached");
    sm64pow::stagec::note_inputs(commit::header_of(&pr).input_hash, &pr.inputs);
    let sn = sm64pow_records_for_peer(sfield.as_ptr(), sfield.len(), std::ptr::null(), std::ptr::null_mut(), 0);
    assert!(sn != 0 && sn != usize::MAX);
    assert_eq!(sm64pow_records_for_peer(std::ptr::null(), 0, std::ptr::null(), std::ptr::null_mut(), 0), 0);
    assert_eq!(sm64pow_records_for_peer([1u8, 2].as_ptr(), 2, std::ptr::null(), std::ptr::null_mut(), 0), usize::MAX);

    // ---- records_ready_at ----
    let tip = [0x42; 32];
    assert_eq!(sm64pow_records_ready_at(tip.as_ptr()), 0, "unknown");
    sm64pow::pof::policy().lock().unwrap().note_eligible(tip, PREV, [1; 32], 2000, 1_800_000_000, 1_800_000_005);
    assert_eq!(sm64pow_records_ready_at(tip.as_ptr()), 1_800_000_005 + sm64pow::rules::POF_WINDOW_SECONDS + 10);

    // ---- subaddress ----
    let a = [5u8; 32];
    let mut a_s = [0u8; 32];
    a_s[0] = 5; // a canonical scalar
    let a_s = a_s;
    let _ = a;
    let b_pub = kp(6).public;
    let (mut d, mut c) = ([0u8; 32], [0u8; 32]);
    assert_eq!(sm64pow_subaddress(a_s.as_ptr(), b_pub.as_ptr(), 0, 0, d.as_mut_ptr(), c.as_mut_ptr()), 1);
    assert_eq!(d, b_pub, "(0,0) is the main address");
    let (mut d1, mut c1) = ([0u8; 32], [0u8; 32]);
    assert_eq!(sm64pow_subaddress(a_s.as_ptr(), b_pub.as_ptr(), 0, 1, d1.as_mut_ptr(), c1.as_mut_ptr()), 1);
    assert!(d1 != d && c1 != c);
    assert_eq!(sm64pow_subaddress([0xff; 32].as_ptr(), b_pub.as_ptr(), 0, 1, d1.as_mut_ptr(), c1.as_mut_ptr()), 0);

    // ---- preverify_records ----
    features::set(Features::STAGE_A).unwrap();
    assert_eq!(preverify(&[], &[], &[], 0, 2), 0, "empty");
    features::set(Features::ALL).unwrap();
    if rom {
        let j: serde_json::Value = serde_json::from_str(include_str!("fixtures/power_on_star.json")).unwrap();
        let seed = sm64pow::Seed::from_hex(j["seed_hash"].as_str().unwrap()).unwrap();
        let old: Vec<u8> = (0..j["proof_hex"].as_str().unwrap().len() / 2).map(|i| u8::from_str_radix(&j["proof_hex"].as_str().unwrap()[2 * i..2 * i + 2], 16).unwrap()).collect();
        let pr = sm64pow::proof::decode_proof(&old, 36_000).unwrap();
        let new = commit::commit_layout(&old, &commit::proof_hash(&pr)).unwrap();
        // B_{h-1}: a real block (its end state is cached); block h and the including block h+1 follow.
        let x = [0x31; 32];
        assert!(matches!(sm64pow::chain::verify_any(&x, sm64pow::chain::Parent::Genesis, &seed, 1, &new).unwrap(), sm64pow::chain::CommittedVerdict::Run(sm64pow::chain::BlockVerdict::Valid { .. })));
        let recs: Vec<Record> = (1..=5).map(|i| record_on(&x, i, 2, 2000 + 20 * i as u32)).collect();
        let (field2, root2) = sorted(recs.clone());
        let mk = |prev: &[u8; 32], key: u8, nonce_root: Option<&[u8; 32]>, field: &[u8]| {
            let mut b = header(prev);
            b.extend(coinbase(3, &records::template_nonce(&kp(key).public, None, nonce_root)));
            b.push(0);
            varint(&mut b, hproof.len() as u64);
            b.extend_from_slice(&hproof);
            b.extend_from_slice(&[0; 64]);
            if !field.is_empty() {
                varint(&mut b, field.len() as u64);
                b.extend_from_slice(field);
            }
            b
        };
        let (bh, bi) = (mk(&x, 100, None, &[]), mk(&[0x32; 32], 101, Some(&root2), &field2));
        let mut ids = vec![0x32u8; 32];
        ids.extend_from_slice(&[0x33; 32]);
        let blobs: Vec<u8> = [bh.clone(), bi.clone()].concat();
        let offs = [0u64, bh.len() as u64, blobs.len() as u64];
        let before = sm64pow::cache::replays();
        let r = sm64pow_preverify_records(2, ids.as_ptr(), blobs.as_ptr(), offs.as_ptr(), [0u64, 0].as_ptr(), 3);
        assert_eq!(r, n as u64, "every record replayed once");
        assert_eq!(sm64pow::cache::replays() - before, n as u64);
        // again: cached verdicts, no replays
        assert_eq!(sm64pow_preverify_records(2, ids.as_ptr(), blobs.as_ptr(), offs.as_ptr(), [0u64, 0].as_ptr(), 3), 0);
        // the sequential check finds the verdicts (they are invalid runs) without replaying
        let h = sm64pow::included::Parent1 { height: 2, prev_id: x, prev_is_genesis: false, nonce: &sm64pow::sign::key_nonce(&kp(100).public), proof: &hproof, difficulty: D };
        let nn = records::template_nonce(&kp(101).public, None, Some(&root2));
        let v = sm64pow::included::verify_block_records(&field2, &nn, &h, 2, &sm64pow::included::Real).unwrap();
        assert!(matches!(v, sm64pow::included::BlockRecords::Invalid(_)), "{v:?}");
        assert_eq!(sm64pow::cache::replays() - before, n as u64, "no second replay");
        // switches off: nothing
        features::set(Features::STAGE_A).unwrap();
        assert_eq!(sm64pow_preverify_records(2, ids.as_ptr(), blobs.as_ptr(), offs.as_ptr(), [0u64, 0].as_ptr(), 3), 0);
        features::set(Features::ALL).unwrap();
    }
    // garbage never panics or replays
    let junk = [1u8, 2, 3, 4];
    assert_eq!(sm64pow_preverify_records(2, [0u8; 64].as_ptr(), junk.as_ptr(), [0u64, 2, 4].as_ptr(), std::ptr::null(), 2), 0);
    features::set(Features::ALL).unwrap();
}

fn preverify(_a: &[u8], _b: &[u8], _c: &[u8], _d: u64, _t: u32) -> u64 {
    sm64pow_preverify_records(0, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), 2)
}
