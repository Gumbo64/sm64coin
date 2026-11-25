//! proofid_vectors_v04 [IN.jsonl] > OUT.jsonl: v0.4 (committed inputs) test vectors for every port of the
//! committed proof blob (docs/V0.4-DESIGN.md section 1.2): the web wallet's C++ `sm64coin::proof_id`
//! (web/wallet-wasm), the Python SDK (python/src/sm64coin/sign.py) and the browser (web/src/lib/block.js).
//!
//! ```text
//! sm64_proof = varint(frames) || start (0|1) || input_hash (32) || codec id || body
//! input_hash = keccak256("sm64run2" || frames u32 LE || start || 4 bytes per non-poked input)
//! proof_id   = input_hash read from the header (no decoding); a header that doesn't parse
//!              (truncated, non-canonical or >10-byte varint, frames > 1<<20, start > 1, < 32 hash bytes):
//!              keccak256("sm64bad!" || blob)
//! ```
//!
//! IN (optional; the same file proofid_vectors.rs takes, `{"name","frames","start","pads"}`) adds real runs; a
//! fixed set of synthetic runs is always emitted. OUT lines:
//! `{"name","blob","id","header":bool,"frames","start","input_hash","pads","stateless":bool}`:
//! `header` whether the committed header parses (then `id` is its hash), `frames`/`start` the header's (when it
//! parses), `stateless` whether `commit::open` decodes the encoding with a stateless codec to the committed hash
//! (a good blob; false for a bad encoding or an invalid header). Run lines (`kind":"run"`) also carry `pads`
//! (hex, 4 bytes per input) and `input_hash` computed from them, for ports that build blobs.
//! v0.3's vectors (proofid_vectors.rs) are unchanged.
use sm64pow::commit::{self, Header};
use sm64pow::proof::{self, pad_from, Proof, Raw, RunLength, Start, Xz};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}
fn varint(mut v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
    out
}

struct Out(Vec<(String, Vec<u8>, Option<Vec<u8>>)>);
impl Out {
    fn blob(&mut self, n: impl Into<String>, b: Vec<u8>) {
        self.0.push((n.into(), b, None));
    }
}

fn run(out: &mut Out, name: &str, pr: &Proof, rawpads: &[u8]) {
    for (tag, c) in [("raw", &Raw as &dyn proof::Codec), ("rle", &RunLength), ("xz", &Xz)] {
        out.0.push((format!("{name}/{tag}"), commit::commit_with(c, pr), Some(rawpads.to_vec())));
    }
    let h = commit::header_of(pr);
    let raw = commit::commit_with(&Raw, pr);
    let mut m = |n: &str, b: Vec<u8>| out.blob(format!("{name}/{n}"), b);
    // Bad encodings: the header parses, so the id is the committed hash whatever follows.
    m("enc-empty", commit::join(&h, &[]));
    m("enc-unknown-codec", commit::join(&h, &[0xee, 1, 2, 3]));
    m("enc-state-codec-garbage", commit::join(&h, &[5, 0x5a, 0xa5]));
    m("enc-raw-partial-pad", raw[..raw.len() - 1].to_vec());
    let mut flip = raw.clone();
    let k = flip.len() - 2;
    flip[k] ^= 1;
    m("enc-raw-bitflip", flip);
    let mut other = pr.clone();
    if let Some(p) = other.inputs.first_mut() {
        p.button ^= 0x10;
    }
    m("enc-other-run", commit::join(&h, &proof::encode_with(&RunLength, &other.inputs)));
    // The header's own fields are committed: a changed hash byte is another id (and a bad encoding).
    let mut hb = raw.clone();
    let at = varint(pr.frames as u64).len() + 1;
    hb[at + 7] ^= 0x80;
    m("hash-byte-flipped", hb);
    // Header faults: the "sm64bad!" id.
    m("bad-start-2", [&varint(pr.frames as u64)[..], &[2], &raw[at..]].concat());
    for n in [0, 1, at, at + 1, at + 31, at + 32] {
        m(&format!("truncated-{n}"), raw[..n].to_vec());
    }
}

fn main() {
    let mut out = Out(Vec::new());
    // Synthetic runs (always): deterministic pads.
    let synth = |frames: u32, n: usize, start: Start, salt: u8| {
        let inputs: Vec<_> = (0..n)
            .map(|i| {
                let v = (i as u8 / 3).wrapping_mul(7).wrapping_add(salt);
                pad_from(&[v & 0x31, v >> 6, v.wrapping_add(3) & 0xf0, v.wrapping_sub(5) & 0x8f])
            })
            .collect();
        Proof { frames, start, inputs }
    };
    for (name, pr) in [
        ("s1-one-frame", synth(1, 1, Start::Parent, 0)),
        ("s2-poked-tail", synth(5, 3, Start::PowerOn, 9)),
        ("s3-127", synth(127, 127, Start::Parent, 1)),
        ("s4-128", synth(128, 120, Start::PowerOn, 2)),
        ("s5-2600", synth(2600, 2550, Start::Parent, 77)),
        ("s6-16384", synth(16384, 300, Start::PowerOn, 200)),
        ("s7-no-inputs", synth(3, 0, Start::Parent, 0)),
    ] {
        let raw: Vec<u8> = pr.inputs.iter().flat_map(proof::pad_bytes).collect();
        run(&mut out, name, &pr, &raw);
    }
    if let Some(path) = std::env::args().nth(1) {
        for line in std::fs::read_to_string(path).unwrap().lines().filter(|l| !l.trim().is_empty()) {
            let j: serde_json::Value = serde_json::from_str(line).unwrap();
            let rawpads = unhex(j["pads"].as_str().unwrap());
            let pr = Proof {
                frames: j["frames"].as_u64().unwrap() as u32,
                start: if j["start"].as_u64().unwrap() == 1 { Start::PowerOn } else { Start::Parent },
                inputs: rawpads.chunks_exact(4).map(pad_from).collect(),
            };
            run(&mut out, j["name"].as_str().unwrap(), &pr, &rawpads);
        }
    }
    // Header boundaries, independent of any run: frames (0, 1<<20, 1<<20 + 1 and varint corner cases),
    // start bytes, and the varint rules (canonical, at most 10 bytes; the 10th byte's high bits fall off a u64).
    let hash: Vec<u8> = (0..32u8).map(|i| i.wrapping_mul(37).wrapping_add(11)).collect();
    let tail = [0u8, 1, 0x80, 0, 0];
    for f in [0u64, 1, 127, 128, 16383, 16384, 36000, 36001, (1 << 20) - 1, 1 << 20, (1 << 20) + 1, 1 << 32, u64::MAX] {
        for s in [0u8, 1, 2, 255] {
            out.blob(format!("hdr/f{f}-s{s}"), [&varint(f)[..], &[s], &hash, &tail].concat());
        }
        out.blob(format!("hdr/f{f}-no-encoding"), [&varint(f)[..], &[0], &hash].concat());
        out.blob(format!("hdr/f{f}-31-hash-bytes"), [&varint(f)[..], &[0], &hash[..31]].concat());
    }
    out.blob("hdr/noncanonical-varint-0x80-00", [&[0x80u8, 0x00, 0][..], &hash, &tail].concat());
    out.blob("hdr/noncanonical-varint-0x81-00", [&[0x81u8, 0x00, 0][..], &hash, &tail].concat());
    out.blob("hdr/noncanonical-varint-0xff-80-00", [&[0xffu8, 0x80, 0x00, 1][..], &hash, &tail].concat());
    out.blob("hdr/varint-10-bytes-wraps-to-0", [&[0x80u8; 9][..], &[0x02, 0], &hash, &tail].concat());
    out.blob("hdr/varint-10-bytes-wraps-to-1", [&[0x81u8], &[0x80u8; 8][..], &[0x02, 1], &hash, &tail].concat());
    out.blob("hdr/varint-10-bytes-top-bit", [&[0x80u8; 9][..], &[0x01, 0], &hash, &tail].concat());
    out.blob("hdr/varint-11-bytes", [&[0x80u8; 10][..], &[0x01, 0], &hash, &tail].concat());
    out.blob("hdr/varint-unterminated", vec![0x80u8; 5]);
    out.blob("hdr/empty", vec![]);
    for (name, blob, pads) in out.0 {
        let hdr = commit::header(&blob);
        let stateless = matches!(commit::open(&blob, sm64pow::proof::MAX_FRAMES as u32, false), Ok(commit::Opened::Run(..)));
        let mut line = format!("{{\"name\":\"{name}\",\"blob\":\"{}\",\"id\":\"{}\",\"header\":{}", hex(&blob), hex(&commit::proof_id(&blob)), hdr.is_ok());
        if let Ok(Header { frames, start, .. }) = hdr {
            line += &format!(",\"frames\":{frames},\"start\":{}", start as u8);
        }
        line += &format!(",\"stateless\":{stateless}");
        if let Some(p) = pads {
            let h = hdr.unwrap();
            assert_eq!(commit::input_hash(h.frames, h.start, &p.chunks_exact(4).map(pad_from).collect::<Vec<_>>()), h.input_hash);
            assert!(stateless, "{name}");
            line += &format!(",\"kind\":\"run\",\"pads\":\"{}\",\"input_hash\":\"{}\"", hex(&p), hex(&h.input_hash));
        }
        println!("{line}}}");
    }
}
