//! proofid_vectors IN.jsonl > OUT.jsonl: test vectors for the web wallet's wasm `sm64coin::proof_id`
//! (web/wallet-wasm/sm64coin_wasm.cpp, which must agree with `proof_id` here for every stateless codec).
//! IN has lines `{"name":..,"frames":N,"start":0|1,"pads":"<hex, 4 bytes per pad>"}` (real chain proofs'
//! decoded inputs, see web/wallet-wasm/test/make-vectors.py). Each is encoded with codecs 0, 1, 2 (plus xz
//! variants: CRC32/no check, preset 1, a trailing stream and trailing garbage) and a set of malformed blobs
//! derived from them. OUT has lines `{"name":..,"blob":hex,"id":hex}` with the id computed natively.
use sm64pow::proof::{encode_proof_with, pad_from, Proof, RunLength, Raw, Start, Xz};
use std::io::Write;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}
fn xz_with(raw: &[u8], check: xz2::stream::Check, preset: u32) -> Vec<u8> {
    let s = xz2::stream::Stream::new_easy_encoder(preset, check).unwrap();
    let mut e = xz2::write::XzEncoder::new_stream(Vec::new(), s);
    e.write_all(raw).unwrap();
    e.finish().unwrap()
}
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}
fn header(frames: u64, start: u8, codec: u8) -> Vec<u8> {
    let mut v = Vec::new();
    put_varint(&mut v, frames);
    v.push(start);
    v.push(codec);
    v
}

fn main() {
    let input = std::fs::read_to_string(std::env::args().nth(1).expect("IN.jsonl")).unwrap();
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    for line in input.lines().filter(|l| !l.trim().is_empty()) {
        let j: serde_json::Value = serde_json::from_str(line).unwrap();
        let name = j["name"].as_str().unwrap().to_string();
        let frames = j["frames"].as_u64().unwrap();
        let start = j["start"].as_u64().unwrap() as u8;
        let rawpads = unhex(j["pads"].as_str().unwrap());
        let pr = Proof { frames: frames as u32, start: if start == 1 { Start::PowerOn } else { Start::Parent }, inputs: rawpads.chunks_exact(4).map(pad_from).collect() };
        let raw = encode_proof_with(&Raw, &pr);
        let rle = encode_proof_with(&RunLength, &pr);
        let xz = encode_proof_with(&Xz, &pr);
        out.push((format!("{name}/raw"), raw.clone()));
        out.push((format!("{name}/rle"), rle));
        out.push((format!("{name}/xz"), xz.clone()));
        let h = header(frames, start, 2);
        let body = &rawpads[..];
        for (tag, check, preset) in [("crc32", xz2::stream::Check::Crc32, 6), ("nocheck", xz2::stream::Check::None, 1), ("crc64p0", xz2::stream::Check::Crc64, 0)] {
            out.push((format!("{name}/xz-{tag}"), [&h[..], &xz_with(body, check, preset)].concat()));
        }
        // malformed (all must hash like Rust does: "sm64bad!" || blob, or, where Rust accepts, the run id)
        let x = &xz[h.len().min(xz.len())..];
        let hh = &xz[..xz.len() - x.len()];
        let mut m = |n: &str, b: Vec<u8>| out.push((format!("{name}/bad-{n}"), b));
        m("xz-truncated", [hh, &x[..x.len() - 1]].concat());
        m("xz-truncated-half", [hh, &x[..x.len() / 2]].concat());
        m("xz-empty-body", hh.to_vec());
        let mut flip = x.to_vec();
        let k = flip.len() / 2;
        flip[k] ^= 0x55;
        m("xz-bitflip", [hh, &flip].concat());
        let mut flip2 = x.to_vec();
        flip2[0] ^= 0xff;
        m("xz-bad-magic", [hh, &flip2].concat());
        let mut tail = x.to_vec();
        let n = tail.len();
        tail[n - 1] ^= 1;
        m("xz-bad-footer", [hh, &tail].concat());
        m("xz-trailing-garbage", [hh, x, &[1, 2, 3, 4, 5, 6, 7, 8]].concat());
        m("xz-second-stream", [hh, x, x].concat());
        if frames > 0 {
            m("xz-frames-too-small", [header(frames - 1, start, 2), x.to_vec()].concat()); // output fills the cap
            m("xz-frames-one-more", [header(frames + 1, start, 2), x.to_vec()].concat()); // fewer pads than frames: fine
        }
        m("xz-pad4", [hh, x, &[0u8; 4][..]].concat());
        m("xz-pad3", [hh, x, &[0u8; 3][..]].concat());
        m("xz-pad4-garbage", [hh, x, &[0u8, 0, 0, 0, 9][..]].concat());
        // two streams of the same pads that fit under the frames bound: the pads twice
        let two = [x, x].concat();
        m("xz-second-fits", [header(2 * (rawpads.len() as u64 / 4) + 5, start, 2), two].concat());
        m("xz-second-fits-bad", [header(2 * (rawpads.len() as u64 / 4) + 5, start, 2), [x, &x[..x.len() - 3]].concat()].concat());
        m("xz-frames-over-max", [header(36001, start, 2), x.to_vec()].concat());
        m("xz-bad-start", [header(frames, 2, 2), x.to_vec()].concat());
        m("xz-partial-pad", [h.clone(), xz_with(&[&rawpads[..], &[7u8][..]].concat(), xz2::stream::Check::Crc64, 6)].concat());
        m("xz-of-garbage", [h.clone(), xz_with(b"hello world", xz2::stream::Check::Crc64, 6)].concat());
        m("raw-frames-over-max", [header(36001, start, 0), rawpads.clone()].concat());
        m("raw-partial-pad", [header(frames, start, 0), rawpads[..rawpads.len().saturating_sub(1)].to_vec()].concat());
        m("unknown-codec", [header(frames, start, 9), rawpads.clone()].concat());
        m("rle-zero-run", [header(frames, start, 1), vec![0, 1, 2, 3, 4]].concat());
    }
    for (name, blob) in out {
        let ok = sm64pow::proof::decode_proof(&blob, 36000).is_ok();
        println!("{{\"name\":\"{}\",\"blob\":\"{}\",\"id\":\"{}\",\"decodes\":{}}}", name, hex(&blob), hex(&sm64pow::proof_id(&blob)), ok);
    }
}
