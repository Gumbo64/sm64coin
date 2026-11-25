//! Codecs 6 (statemix2 polar) and 7 (statemix2 square): registered for committed inputs only
//! (v0.3 nodes keep rejecting them), decoded by the replay like 3-5, and what sealing picks.
//! The switches are process-wide: one test, its own process, the cases in sequence. The golden
//! vectors of the codecs themselves are in pow/statecodec (mix2.rs).

use sm64pow::Seed;
use sm64pow::chain::{BlockVerdict, CommittedVerdict, Parent, verify_any, verify_committed_block};
use sm64pow::commit;
use sm64pow::features::{self, Features};
use sm64pow::proof::{self, Proof, Start};
use sm64pow::statecodec::{encode_proof_states, seal_codecs, state_codec};
use sm64pow::{params, played_from_proof};

fn unhex(h: &str) -> Vec<u8> {
    (0..h.len() / 2).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn valid(v: &CommittedVerdict) -> bool {
    matches!(v, CommittedVerdict::Run(BlockVerdict::Valid { .. }))
}

#[test]
fn codecs_6_and_7_are_v04_only_and_verify_committed() {
    // The registry, v0.3 then v0.4 (no ROM needed).
    features::set(Features::NONE).unwrap();
    for id in [3, 4, 5] {
        assert!(state_codec(id).is_some(), "{id} in v0.3");
    }
    for id in [6, 7] {
        assert!(state_codec(id).is_none(), "{id} is not a v0.3 codec");
    }
    assert_eq!(seal_codecs().iter().map(|c| c.id()).collect::<Vec<_>>(), [4, 5], "v0.3 seals as before");
    features::set(Features::STAGE_A).unwrap();
    for id in 3..=7 {
        assert_eq!(state_codec(id).unwrap().id(), id);
    }
    assert!(state_codec(8).is_none() && state_codec(2).is_none());
    assert_eq!(seal_codecs().iter().map(|c| c.id()).collect::<Vec<_>>(), [3, 4, 5, 6, 7], "v0.4 seals with every state codec");

    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    let Ok(rom) = std::fs::read(&rom) else {
        eprintln!("skipping the replay part: no ROM (set SM64COIN_ROM)");
        return;
    };
    sm64pow::init(rom).unwrap();
    let j: serde_json::Value = serde_json::from_str(include_str!("fixtures/power_on_star.json")).unwrap();
    let seed = Seed::from_hex(j["seed_hash"].as_str().unwrap()).unwrap();
    let pr: Proof = proof::decode_proof(&unhex(j["proof_hex"].as_str().unwrap()), 36_000).unwrap();
    let p = params(1);

    // One replay, every state codec; each blob round-trips and verifies as a committed proof.
    let codecs: Vec<_> = (3..=7).map(|id| state_codec(id).unwrap()).collect();
    let blobs = encode_proof_states(&codecs, None, &seed, &p, &pr).unwrap();
    let hash = commit::proof_hash(&pr);
    let want_id = commit::proof_id(&commit::commit_with(&proof::RunLength, &pr));
    let mut sizes = vec![];
    for (c, blob) in codecs.iter().zip(&blobs) {
        // The same bytes as a replay per codec.
        assert_eq!(&sm64pow::statecodec::encode_proof_state(*c, None, &seed, &p, &pr).unwrap(), blob, "codec {}", c.id());
        assert_eq!(proof::split_blob(blob).unwrap().2, c.id());
        let committed = commit::commit_layout(blob, &hash).unwrap();
        assert_eq!(commit::proof_id(&committed), want_id, "the id does not depend on the encoding");
        assert_eq!(sm64pow::decode_proof_any(&committed, None, &seed, &p).unwrap(), pr, "codec {}", c.id());
        let v = verify_committed_block(&[c.id(); 32], Parent::Genesis, &seed, 1, &committed).unwrap();
        assert!(valid(&v), "codec {}: {v:?}", c.id());
        // A body that does not decode to the committed inputs is a bad encoding, not an invalid run.
        let mut bad = committed.clone();
        let n = bad.len();
        bad[n / 2] ^= 0x10;
        let v = verify_committed_block(&[0x70; 32], Parent::Genesis, &seed, 1, &bad).unwrap();
        assert!(matches!(v, CommittedVerdict::BadEncoding(_)), "codec {}: {v:?}", c.id());
        sizes.push((c.id(), blob.len()));
    }
    eprintln!("power-on star ({} frames): xz {} bytes, runlength {}, state codecs {sizes:?}", pr.frames, proof::encode_proof_with(&proof::Xz, &pr).len(), proof::encode_proof_with(&proof::RunLength, &pr).len());

    // Sealing keeps the smallest over the stateless codecs and 3-7, under the same committed header.
    let played = played_from_proof(&seed, &p, &pr).unwrap();
    let sealed = sm64pow::seal_proof(&seed, &p, &played).unwrap();
    let (hdr, enc) = commit::split(&sealed).unwrap();
    assert_eq!(hdr.input_hash, hash);
    let best = sizes.iter().map(|s| s.1).chain([proof::encode_proof(&pr).len()]).min().unwrap();
    assert_eq!(commit::layout(&sealed).unwrap().len(), best, "sealed {} bytes of encoding, best {best}", enc.len());
    assert!(valid(&verify_committed_block(&[0x71; 32], Parent::Genesis, &seed, 1, &sealed).unwrap()));

    // v0.3 nodes: 6 and 7 are unknown ids, the blob does not verify.
    features::set(Features::NONE).unwrap();
    for (c, blob) in codecs.iter().zip(&blobs) {
        let v = verify_any(&[0x72; 32], Parent::Genesis, &seed, 1, blob).unwrap();
        match c.id() {
            3..=5 => assert!(valid(&v), "codec {} in v0.3: {v:?}", c.id()),
            _ => assert!(!valid(&v), "codec {} must not verify in v0.3", c.id()),
        }
    }
    let _ = Start::PowerOn;
    features::set(Features::ALL).unwrap();
}
