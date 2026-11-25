//! sig_vectors: prints signing vectors as JSON lines `{entropy, secret, public, extra_nonce, seed, proof, proof_id, frames, sig}`
//! (hex). Signing is deterministic, so the browser path (pow/webminer's `miner_keypair` / `sign_proof` in WASM, called by
//! web/src/lib/minerkey.js) must reproduce every `public`, `extra_nonce` and `sig` exactly: feed it `entropy`, `seed`, `proof`.
//! Each vector's signature is also checked with sign::check. Run: `cargo run --example sig_vectors`.
use sm64pow::sign::{self, Keypair};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A raw (codec 0) proof blob of `frames` unpoked pads, as web/src/lib/proof.js proofFromRun writes it.
fn raw_proof(frames: u32, start: u8, salt: u8) -> Vec<u8> {
    let mut b = Vec::new();
    let mut n = frames;
    while n >= 0x80 {
        b.push((n as u8 & 0x7f) | 0x80);
        n >>= 7;
    }
    b.push(n as u8);
    b.push(start);
    b.push(0); // codec: raw
    for i in 0..frames {
        let v = (i as u8).wrapping_mul(7).wrapping_add(salt);
        b.extend_from_slice(&[v, (v >> 1) & 1, v.wrapping_add(3), v.wrapping_sub(5)]);
    }
    b
}

fn main() {
    let cases: [([u8; 32], u32, u8, u8); 4] = [([1; 32], 5, 1, 0), ([0xab; 32], 300, 0, 9), ([0xff; 32], 1, 1, 2), (std::array::from_fn(|i| i as u8), 2000, 1, 77)];
    for (entropy, frames, start, salt) in cases {
        let k = Keypair::from_entropy(&entropy).unwrap();
        let seed: [u8; 32] = sign::cn_fast_hash(&entropy);
        let proof = raw_proof(frames, start, salt);
        let sig = sm64pow::mining::sign_proof(&k, &seed, &proof).unwrap();
        let pid = sm64pow::proof_id::proof_id(&proof);
        assert_eq!(sign::check(&seed, &k.extra_nonce(), &pid, frames, &sig), Ok(k.public));
        println!(
            "{{\"entropy\":\"{}\",\"secret\":\"{}\",\"public\":\"{}\",\"extra_nonce\":\"{}\",\"seed\":\"{}\",\"proof\":\"{}\",\"proof_id\":\"{}\",\"frames\":{},\"sig\":\"{}\"}}",
            hex(&entropy), hex(&k.secret), hex(&k.public), hex(&k.extra_nonce()), hex(&seed), hex(&proof), hex(&pid), frames, hex(&sig)
        );
    }
}
