//! Block templates as self-contained evidence: from the pieces of a block
//! (header, coinbase, other tx hashes) compute the block's seed and read the
//! miner key committed in the coinbase, with no block at hand. Mirrors
//! Monero's `get_block_hashing_blob`, `tree_hash`, `parse_tx_extra` and the
//! daemon's `sm64_seed` / `sm64_miner_nonce`.

use crate::sign::{cn_fast_hash, miner_key};

pub const MAX_HEADER: usize = 128;
pub const MAX_MINER_TX: usize = 4096;
pub const MAX_TX_HASHES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateParts {
    pub header: Vec<u8>,
    pub miner_tx: Vec<u8>,
    pub tx_hashes: Vec<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateFacts {
    pub seed: [u8; 32],
    pub key: Option<[u8; 32]>,
    pub prev_id: [u8; 32],
    pub height: u64,
    pub timestamp: u64,
}

const E: &str = "malformed template";

/// Cursor over a byte slice with Monero's canonical varints.
struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn eof(&self) -> bool {
        self.p >= self.b.len()
    }
    fn u8(&mut self) -> Result<u8, &'static str> {
        let v = *self.b.get(self.p).ok_or(E)?;
        self.p += 1;
        Ok(v)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let end = self.p.checked_add(n).filter(|&e| e <= self.b.len()).ok_or(E)?;
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn h32(&mut self) -> Result<[u8; 32], &'static str> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    /// Canonical LEB128 (no trailing zero group, at most 63 bits like Monero's u64 reader).
    fn varint(&mut self) -> Result<u64, &'static str> {
        let mut v = 0u64;
        for i in 0..10 {
            let c = self.u8()?;
            let low = (c & 0x7f) as u64;
            if i == 9 && low > 1 {
                return Err(E);
            }
            v |= low << (7 * i);
            if c & 0x80 == 0 {
                if c == 0 && i > 0 {
                    return Err(E);
                }
                return Ok(v);
            }
        }
        Err(E)
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Header: major, minor, timestamp, prev_id, nonce. Returns (timestamp, prev_id, end of the timestamp varint start).
fn parse_header(h: &[u8]) -> Result<(u64, [u8; 32], usize), &'static str> {
    let mut r = Rd { b: h, p: 0 };
    r.varint()?;
    r.varint()?;
    let ts_at = r.p;
    let ts = r.varint()?;
    let prev = r.h32()?;
    r.take(4)?;
    if !r.eof() {
        return Err(E);
    }
    Ok((ts, prev, ts_at))
}

/// Parses a v2 coinbase at the start of `t`: (total length, prefix length, height, extra).
fn parse_miner_tx(t: &[u8]) -> Result<(usize, usize, u64, &[u8]), &'static str> {
    let mut r = Rd { b: t, p: 0 };
    if r.varint()? != 2 {
        return Err(E);
    }
    r.varint()?; // unlock_time
    if r.varint()? != 1 || r.u8()? != 0xff {
        return Err(E);
    }
    let height = r.varint()?;
    let nout = r.varint()?;
    for _ in 0..nout {
        r.varint()?;
        match r.u8()? {
            0x02 => drop(r.take(32)?),
            0x03 => drop(r.take(33)?),
            _ => return Err(E),
        }
    }
    let n = r.varint()? as usize;
    let extra = r.take(n)?;
    let prefix = r.p;
    if r.u8()? != 0 {
        return Err(E); // RCTTypeNull
    }
    Ok((r.p, prefix, height, extra))
}

/// Splits a full SM64Coin block blob into its parts (and ignores the trailing sm64 proof and signature).
pub fn split_block(blob: &[u8]) -> Result<TemplateParts, &'static str> {
    let mut r = Rd { b: blob, p: 0 };
    r.varint()?;
    r.varint()?;
    r.varint()?;
    r.take(36)?;
    let header = blob[..r.p].to_vec();
    let start = r.p;
    let (len, ..) = parse_miner_tx(&blob[start..])?;
    let miner_tx = blob[start..start + len].to_vec();
    r.p = start + len;
    let n = r.varint()? as usize;
    if n > MAX_TX_HASHES {
        return Err(E);
    }
    let tx_hashes = (0..n).map(|_| r.h32()).collect::<Result<Vec<_>, _>>()?;
    let plen = r.varint()? as usize;
    r.take(plen)?;
    r.take(64)?;
    if !r.eof() || header.len() > MAX_HEADER || miner_tx.len() > MAX_MINER_TX {
        return Err(E);
    }
    Ok(TemplateParts { header, miner_tx, tx_hashes })
}

/// Monero's `tree_hash` (src/crypto/tree-hash.c).
pub fn tree_hash(h: &[[u8; 32]]) -> [u8; 32] {
    let pair = |a: &[u8; 32], b: &[u8; 32]| {
        let mut x = [0u8; 64];
        x[..32].copy_from_slice(a);
        x[32..].copy_from_slice(b);
        cn_fast_hash(&x)
    };
    match h.len() {
        0 => [0; 32],
        1 => h[0],
        2 => pair(&h[0], &h[1]),
        count => {
            let mut cnt = 2;
            while cnt < count {
                cnt <<= 1;
            }
            cnt >>= 1;
            let k = 2 * cnt - count;
            let mut ints = vec![[0u8; 32]; cnt];
            ints[..k].copy_from_slice(&h[..k]);
            let mut i = k;
            for j in k..cnt {
                ints[j] = pair(&h[i], &h[i + 1]);
                i += 2;
            }
            while cnt > 2 {
                cnt >>= 1;
                for j in 0..cnt {
                    ints[j] = pair(&ints[2 * j], &ints[2 * j + 1]);
                }
            }
            pair(&ints[0], &ints[1])
        }
    }
}

/// The first extra nonce that parses (fields before a parse error still count).
fn first_extra_nonce(extra: &[u8]) -> Option<&[u8]> {
    let mut r = Rd { b: extra, p: 0 };
    while !r.eof() {
        let f = (|| -> Result<Option<&[u8]>, &'static str> {
            match r.u8()? {
                0x00 => {
                    let mut k = 0;
                    while !r.eof() {
                        if r.u8()? != 0 {
                            return Err(E);
                        }
                        k += 1;
                        if k > 254 {
                            return Err(E);
                        }
                    }
                }
                0x01 => drop(r.take(32)?),
                0x02 => {
                    let n = r.varint()? as usize;
                    if n > 255 {
                        return Err(E);
                    }
                    return Ok(Some(r.take(n)?));
                }
                0x03 => {
                    let n = r.varint()? as usize;
                    let mut m = Rd { b: r.take(n)?, p: 0 };
                    m.varint()?;
                    m.h32()?;
                }
                0x04 => {
                    let n = r.varint()? as usize;
                    r.take(n.checked_mul(32).ok_or(E)?)?;
                }
                0xde => {
                    let n = r.varint()? as usize;
                    r.take(n)?;
                }
                _ => return Err(E),
            }
            Ok(None)
        })();
        match f {
            Ok(Some(n)) => return Some(n),
            Ok(None) => {}
            Err(_) => return None,
        }
    }
    None
}

/// The facts: seed, the committed miner key, prev id, height (from the coinbase's txin_gen), timestamp.
pub fn facts(p: &TemplateParts) -> Result<TemplateFacts, &'static str> {
    if p.header.len() > MAX_HEADER || p.miner_tx.len() > MAX_MINER_TX || p.tx_hashes.len() > MAX_TX_HASHES {
        return Err(E);
    }
    let (timestamp, prev_id, ts_at) = parse_header(&p.header)?;
    let (len, prefix, height, extra) = parse_miner_tx(&p.miner_tx)?;
    if len != p.miner_tx.len() {
        return Err(E);
    }
    // Monero v2 / RCTTypeNull: H(H(prefix) || H(0x00) || 32 zeros)
    let mut x = Vec::with_capacity(96);
    x.extend_from_slice(&cn_fast_hash(&p.miner_tx[..prefix]));
    x.extend_from_slice(&cn_fast_hash(&[0]));
    x.extend_from_slice(&[0; 32]);
    let mut leaves = vec![cn_fast_hash(&x)];
    leaves.extend_from_slice(&p.tx_hashes);
    // hashing blob with timestamp 0
    let mut r = Rd { b: &p.header, p: ts_at };
    r.varint()?;
    let mut hb = p.header[..ts_at].to_vec();
    hb.push(0);
    hb.extend_from_slice(&p.header[r.p..]);
    hb.extend_from_slice(&tree_hash(&leaves));
    put_varint(&mut hb, leaves.len() as u64);
    let key = first_extra_nonce(extra).and_then(miner_key);
    Ok(TemplateFacts { seed: cn_fast_hash(&hb), key, prev_id, height, timestamp })
}

/// Compact self-delimiting serialization (varint lengths), for a P2P message.
pub fn encode(p: &TemplateParts) -> Vec<u8> {
    let mut o = Vec::with_capacity(p.header.len() + p.miner_tx.len() + 32 * p.tx_hashes.len() + 8);
    put_varint(&mut o, p.header.len() as u64);
    o.extend_from_slice(&p.header);
    put_varint(&mut o, p.miner_tx.len() as u64);
    o.extend_from_slice(&p.miner_tx);
    put_varint(&mut o, p.tx_hashes.len() as u64);
    for h in &p.tx_hashes {
        o.extend_from_slice(h);
    }
    o
}

/// Inverse of [`encode`]; also returns the bytes consumed.
pub fn decode(b: &[u8]) -> Result<(TemplateParts, usize), &'static str> {
    let mut r = Rd { b, p: 0 };
    let n = r.varint()? as usize;
    if n > MAX_HEADER {
        return Err(E);
    }
    let header = r.take(n)?.to_vec();
    let n = r.varint()? as usize;
    if n > MAX_MINER_TX {
        return Err(E);
    }
    let miner_tx = r.take(n)?.to_vec();
    let n = r.varint()? as usize;
    if n > MAX_TX_HASHES {
        return Err(E);
    }
    let tx_hashes = (0..n).map(|_| r.h32()).collect::<Result<Vec<_>, _>>()?;
    Ok((TemplateParts { header, miner_tx, tx_hashes }, r.p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sign::Keypair;

    const PREV: &str = "cbd56c9bf4ea17392ccfdb7a661f0e3e449b55b95a9a2e1eeea426132dfbde9f";
    // (template_blob, seed_hash, key_secret)
    const CASES: [(&str, &str, &str); 4] = [
        ("1010cbc79cd806cbd56c9bf4ea17392ccfdb7a661f0e3e449b55b95a9a2e1eeea426132dfbde9f00000000020501ff01018080808080feff0103625bfb11b4df4604cb9e4f48d1cba9659afc7c5d30b15201abf3f947d8dc6ca1964401d188f1efb7f2357c88faa782cfd79d9cea1efe0bf5c9a81af6b187dc43e711ff0221642bdcdddf40937bd9ba4101bc9e83a966964c9f39b785d1018f0d2883b1394b4400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
         "f65df5913eb03cc17a9ec589c9a10474ae24ff21714d8bf9fc0948bf027335d4", "997a5ae58be755d2223b3d664cdbcb38fa07cd1d7abd63af92c9740e9bbccd05"),
        ("1010989b9bd806cbd56c9bf4ea17392ccfdb7a661f0e3e449b55b95a9a2e1eeea426132dfbde9f00000000020501ff01018080808080feff010381f8fd65877f1ff7c6aab1f72aa1fef0944193469e8d0ccede0881e5e6088f76e944018aec2f0ed0c15187fbbc1a5ea511b15b750dce2d6206f3b0e46073d9f29da0b2022164ff6c4f156fb4ea0af27f8cb1796499ad598bcb945f95bfc240b3f439a1994d3700000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
         "37a64aa4c74d1ab66043a36f089cefa2ef3b308dbebc46bdc45c7356ad4be99e", "ab14ff67a443e4be0b744784d6fd39203bebc52406e39be4428409c3a02aa603"),
        ("1010cbc79cd806cbd56c9bf4ea17392ccfdb7a661f0e3e449b55b95a9a2e1eeea426132dfbde9f00000000020501ff01018080808080feff01039104d7da53e0617d793c43dc07cc05c930db0173bacd504f94efe2193b04dea93a44019bad870f0d0a9684b9bee687e585e4c3f70d3c93b3ce06effa436b9672cdda01022164343f896777063a980d337770fc0f035c0f7583eba9d1b1ed6be26b49032051bf00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
         "ebb48f9361cb2f760f893e39c0a69c290759bc77371b6db1e8d95006cf6d6507", "f09cf5fe839a298afb6154783ae07874f9e61e9d9793d2ebb23fcb945f50290e"),
        ("1010989b9bd806cbd56c9bf4ea17392ccfdb7a661f0e3e449b55b95a9a2e1eeea426132dfbde9f00000000020501ff01018080808080feff0103509d08a7999e093471d60be01702071ec263e81afbc355388d583f7ff560303116440186a55516f96750ec5929e8a9f1ea713fc666e6bbc4c3a263ab8eb68cf90714d50221646fdb1f353006904ea5e951a9e92070f754155c491cb716a2135478340c6a97e800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
         "44bb768d7634eff663e1e9c8f4b7049167401269394f6caea96726c1e7b50b4f", "f0a62e543ad5b914b4fe438b05b3fed6f0dc1421caa3b9c56488a5b0bbf8990d"),
    ];
    const PUBS: [&str; 4] = [
        "2bdcdddf40937bd9ba4101bc9e83a966964c9f39b785d1018f0d2883b1394b44",
        "ff6c4f156fb4ea0af27f8cb1796499ad598bcb945f95bfc240b3f439a1994d37",
        "343f896777063a980d337770fc0f035c0f7583eba9d1b1ed6be26b49032051bf",
        "6fdb1f353006904ea5e951a9e92070f754155c491cb716a2135478340c6a97e8",
    ];

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }
    fn a32(s: &str) -> [u8; 32] {
        unhex(s).try_into().unwrap()
    }

    #[test]
    fn real_templates() {
        for (i, (blob, seed, secret)) in CASES.iter().enumerate() {
            let p = split_block(&unhex(blob)).unwrap();
            let f = facts(&p).unwrap();
            assert_eq!(f.seed, a32(seed), "seed {i}");
            let kp = Keypair::from_secret(&a32(secret)).unwrap();
            assert_eq!(kp.public, a32(PUBS[i]));
            assert_eq!(f.key, Some(kp.public), "key {i}");
            assert_eq!((f.height, f.prev_id), (1, a32(PREV)));
            assert!(f.timestamp > 0);
        }
    }

    #[test]
    fn roundtrip_and_tamper() {
        let mut p = split_block(&unhex(CASES[0].0)).unwrap();
        p.tx_hashes = vec![[7; 32], [9; 32]];
        let e = encode(&p);
        let mut padded = e.clone();
        padded.extend_from_slice(b"tail");
        let (q, n) = decode(&padded).unwrap();
        assert_eq!((q.clone(), n), (p.clone(), e.len()));
        let s = facts(&p).unwrap().seed;
        let mut t = p.clone();
        let n = t.miner_tx.len();
        t.miner_tx[n - 3] ^= 1; // inside extra
        assert_ne!(facts(&t).unwrap().seed, s);
        let mut t = p.clone();
        t.tx_hashes[1][0] ^= 1;
        assert_ne!(facts(&t).unwrap().seed, s);
        // the seed ignores the timestamp
        let mut t = p.clone();
        t.header[2] ^= 1;
        assert_eq!(facts(&t).unwrap().seed, s);
    }

    /// Straight port of tree-hash.c for the 3/4/5/9 cases, against hand-folded trees.
    #[test]
    fn tree_hash_shapes() {
        let h: Vec<[u8; 32]> = (0..9u8).map(|i| [i + 1; 32]).collect();
        let c = |a: &[u8; 32], b: &[u8; 32]| cn_fast_hash(&[a.as_slice(), b.as_slice()].concat());
        assert_eq!(tree_hash(&h[..1]), h[0]);
        assert_eq!(tree_hash(&h[..2]), c(&h[0], &h[1]));
        assert_eq!(tree_hash(&h[..3]), c(&h[0], &c(&h[1], &h[2])));
        assert_eq!(tree_hash(&h[..4]), c(&c(&h[0], &h[1]), &c(&h[2], &h[3])));
        // 5: cnt 4, k=3: leaves h0 h1 h2 then H(h3,h4)
        assert_eq!(tree_hash(&h[..5]), c(&c(&h[0], &h[1]), &c(&h[2], &c(&h[3], &h[4]))));
        // 9: cnt 8, k=7: h0..h6 then H(h7,h8)
        let l = [c(&c(&h[0], &h[1]), &c(&h[2], &h[3])), c(&c(&h[4], &h[5]), &c(&h[6], &c(&h[7], &h[8])))];
        assert_eq!(tree_hash(&h), c(&l[0], &l[1]));
    }

    #[test]
    fn extra_parsing() {
        let k = Keypair::from_secret(&a32(CASES[0].2)).unwrap().public;
        let mut nonce = vec![2, 33, 0x64];
        nonce.extend_from_slice(&k);
        assert_eq!(first_extra_nonce(&nonce), Some(&nonce[2..]));
        // a bad field after the nonce still leaves the nonce; one before it hides it
        let mut after = nonce.clone();
        after.extend_from_slice(&[0x77, 1]);
        assert!(first_extra_nonce(&after).is_some());
        let mut before = vec![0x77];
        before.extend_from_slice(&nonce);
        assert!(first_extra_nonce(&before).is_none());
        // nonce over 255 bytes is rejected (varint 0x80 0x02 = 256)
        assert!(first_extra_nonce(&[2, 0x80, 2]).is_none());
        assert!(first_extra_nonce(&[0, 0, 5]).is_none());
    }

    #[test]
    fn truncations_never_panic() {
        let blob = unhex(CASES[1].0);
        for n in 0..blob.len() {
            assert!(split_block(&blob[..n]).is_err(), "split {n}");
        }
        let e = encode(&split_block(&blob).unwrap());
        for n in 0..e.len() {
            assert!(decode(&e[..n]).is_err(), "decode {n}");
        }
        let p = split_block(&blob).unwrap();
        for n in 0..p.miner_tx.len() {
            let mut t = p.clone();
            t.miner_tx.truncate(n);
            assert!(facts(&t).is_err());
        }
        for n in 0..p.header.len() {
            let mut t = p.clone();
            t.header.truncate(n);
            assert!(facts(&t).is_err());
        }
        let mut big = p.clone();
        big.tx_hashes = vec![[0; 32]; MAX_TX_HASHES + 1];
        assert!(facts(&big).is_err() && decode(&encode(&big)).is_err());
    }
}
