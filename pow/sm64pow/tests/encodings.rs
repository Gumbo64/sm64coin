//! One block id, several encodings (review: the block id commits to proof_id, not to the blob).
//! For the stateless codecs (0 raw, 1 run-length, 2 xz) proof_id is the decoded run, so the same
//! block can travel in different encodings. A bad encoding must never poison the id: either it
//! doesn't decode to the run (then its proof_id, hence its block id, is a different one), or it
//! is judged by the decoded run like every other encoding. Its own process (it counts on the cache).

use sm64pow::Seed;
use sm64pow::chain::{BlockVerdict, Parent, verify_block};
use sm64pow::proof::{RunLength, Raw, decode_proof, encode_proof_with};

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

fn fixture() -> (Seed, Vec<u8>) {
    let j: serde_json::Value = serde_json::from_str(include_str!("fixtures/power_on_star.json")).unwrap();
    let seed = Seed::from_hex(j["seed_hash"].as_str().unwrap()).unwrap();
    let h = j["proof_hex"].as_str().unwrap();
    (seed, (0..h.len() / 2).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()).collect())
}

#[test]
fn a_bad_encoding_first_never_blocks_the_good_one() {
    if !init() {
        return;
    }
    let (seed, xz) = fixture(); // codec 2 (xz)
    let run = decode_proof(&xz, 36000).unwrap();
    let raw = encode_proof_with(&Raw, &run);
    let rle = encode_proof_with(&RunLength, &run);
    let id = sm64pow::proof_id::proof_id(&xz);
    assert!(sm64pow::proof_id::proof_id(&raw) == id && sm64pow::proof_id::proof_id(&rle) == id, "one block id for every encoding of the run");
    // The largest valid run's raw encoding fits the daemon's blob cap (SM64COIN_MAX_PROOF_BYTES,
    // 256 KiB, checked when the block is parsed, before any id exists): an over-size blob is a
    // bloated encoding that is refused as unparseable, never recorded as an invalid block id.
    assert!(sm64pow::rules().max_frames_any() as usize * 4 + 16 < 256 * 1024);
    // Encodings that don't decode to the run have another proof_id, hence another block id.
    let mut corrupt = xz.clone();
    let n = corrupt.len();
    corrupt[n - 20] ^= 0xff; // inside the xz stream (its CRC no longer matches)
    let mut garbage = xz[..4].to_vec(); // the header (frames, start, codec) and a junk body
    garbage.extend_from_slice(&[0x55; 40]);
    let mut bigdict = xz.clone(); // a dictionary over the decoder's memory limit
    {
        let body = &mut bigdict[4..];
        if body.len() > 24 && body[12 + 2] == 0x21 {
            body[12 + 4] = 0x28;
            let mut crc = !0u32;
            for &b in &body[12..12 + 8] {
                crc ^= b as u32;
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
                }
            }
            body[12 + 8..12 + 12].copy_from_slice(&(!crc).to_le_bytes());
        }
    }
    for (what, bad) in [("corrupt xz", &corrupt), ("garbage body", &garbage), ("huge dictionary", &bigdict)] {
        assert_ne!(sm64pow::proof_id::proof_id(bad), id, "{what}: must not share the run's id");
    }
    // Even under the run's id (as if a peer lied about it), a bad encoding judged first doesn't
    // make the good encodings fail: the invalid-run cache is keyed by the blob, not the block id.
    let bid = [0x5a; 32];
    for bad in [&corrupt, &garbage, &bigdict] {
        assert!(matches!(verify_block(&bid, Parent::Genesis, &seed, 1, bad).unwrap(), BlockVerdict::Invalid(_)));
    }
    for good in [&raw, &rle, &xz] {
        assert!(matches!(verify_block(&bid, Parent::Genesis, &seed, 1, good).unwrap(), BlockVerdict::Valid { .. }), "a good encoding after bad ones");
    }
}

/// The committed proof-id vectors (web/wallet-wasm/test/vectors.jsonl, which the wallet's C++ port
/// is checked against) still match the native ids (e.g. after a decoder limit change).
#[test]
fn the_committed_proof_id_vectors_still_match() {
    if !init() {
        return;
    }
    // v0.3's vectors: v0.3's registry (codecs 6-7 exist only with committed inputs, where they are
    // transport). Both tests of this file run v0.3 semantics, so the switch can't upset the other.
    sm64pow::features::set(sm64pow::features::Features::NONE).unwrap();
    let mut n = 0;
    for l in include_str!("../../../web/wallet-wasm/test/vectors.jsonl").lines() {
        let j: serde_json::Value = serde_json::from_str(l).unwrap();
        let unhex = |s: &str| -> Vec<u8> { (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect() };
        let blob = unhex(j["blob"].as_str().unwrap());
        let id: String = sm64pow::proof_id::proof_id(&blob).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(id, j["id"].as_str().unwrap(), "{}", j["name"]);
        n += 1;
    }
    assert_eq!(n, 234);
}

/// The v0.4 committed proof-id vectors (web/wallet-wasm/test/vectors-v04.jsonl, which the C++,
/// Python and browser ports are checked against) match commit::proof_id; no game needed.
#[test]
fn the_v04_proof_id_vectors_still_match() {
    let mut n = 0;
    for l in include_str!("../../../web/wallet-wasm/test/vectors-v04.jsonl").lines() {
        let j: serde_json::Value = serde_json::from_str(l).unwrap();
        let unhex = |s: &str| -> Vec<u8> { (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect() };
        let blob = unhex(j["blob"].as_str().unwrap());
        let id: String = sm64pow::commit::proof_id(&blob).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(id, j["id"].as_str().unwrap(), "{}", j["name"]);
        assert_eq!(sm64pow::commit::header(&blob).is_ok(), j["header"].as_bool().unwrap(), "{}", j["name"]);
        n += 1;
    }
    assert_eq!(n, 257);
}
