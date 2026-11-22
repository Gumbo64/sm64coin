//! The coinbase tag record and its verification.
//!
//! Record (all in the coinbase `tx_extra` nonce, 96 bytes at most):
//! ```text
//!   "SM64LB1"          7 bytes  magic + version
//!   name_len           1 byte   1..=24
//!   name               name_len bytes, UTF-8, no control characters
//!   public spend key   32 bytes (the address's B)
//!   private view key   32 bytes (a, a reduced scalar; the address's A must equal a*G)
//! ```
use crate::crypto::*;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};

pub const MAGIC: &[u8; 7] = b"SM64LB1";
pub const MAX_NAME: usize = 24;

#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub name: String,
    pub spend: [u8; 32],
    pub view_secret: [u8; 32],
}

/// A valid display name: 1..=24 bytes of UTF-8 without control characters.
pub fn clean_name(name: &str) -> Result<String, String> {
    let n = name.trim();
    if n.is_empty() || n.len() > MAX_NAME || n.chars().any(|c| c.is_control()) {
        return Err(format!("name must be 1-{MAX_NAME} bytes of printable UTF-8"));
    }
    Ok(n.to_string())
}

impl Tag {
    /// Build the tag for `address` (a standard SM64Coin address), checking that the view
    /// key belongs to it.
    pub fn new(name: &str, address: &str, view_key_hex: &str) -> Result<Tag, String> {
        let name = clean_name(name)?;
        let a = decode_address(address)?;
        let vk = parse_view_key(view_key_hex)?;
        if !view_key_matches(&a, &vk) {
            return Err("view key does not match the address".into());
        }
        Ok(Tag { name, spend: a.spend, view_secret: vk.to_bytes() })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut o = MAGIC.to_vec();
        o.push(self.name.len() as u8);
        o.extend(self.name.as_bytes());
        o.extend(self.spend);
        o.extend(self.view_secret);
        o
    }

    /// Parse a nonce as a tag (None when it is not one: wrong magic, bad lengths, bad UTF-8).
    pub fn decode(b: &[u8]) -> Option<Tag> {
        let rest = b.strip_prefix(&MAGIC[..])?;
        let (&n, rest) = rest.split_first()?;
        let n = n as usize;
        if n == 0 || n > MAX_NAME || rest.len() != n + 64 {
            return None;
        }
        let name = std::str::from_utf8(&rest[..n]).ok()?;
        if name != clean_name(name).ok()? {
            return None;
        }
        let spend: [u8; 32] = rest[n..n + 32].try_into().ok()?;
        let view_secret: [u8; 32] = rest[n + 32..].try_into().ok()?;
        Some(Tag { name: name.to_string(), spend, view_secret })
    }

    /// The address's public view key A = a*G, if the view key is a reduced scalar.
    pub fn view_public(&self) -> Option<[u8; 32]> {
        let a = parse_view_key(&hex::encode(self.view_secret)).ok()?;
        Some((a * curve25519_dalek::constants::ED25519_BASEPOINT_POINT).compress().to_bytes())
    }

    pub fn address(&self) -> Option<String> {
        Some(encode_address(&Address { prefix: PREFIX_STANDARD, spend: self.spend, view: self.view_public()? }))
    }
}

/// What a coinbase's extra holds: the tx public key and the nonces.
#[derive(Debug, Default, PartialEq)]
pub struct Extra {
    pub tx_pubkey: Option<[u8; 32]>,
    pub nonces: Vec<Vec<u8>>,
}

fn varint_at(b: &[u8]) -> Option<(usize, usize)> {
    let mut v = 0usize;
    for (j, &x) in b.iter().enumerate().take(5) {
        v |= ((x & 0x7f) as usize) << (7 * j);
        if x & 0x80 == 0 {
            return Some((v, j + 1));
        }
    }
    None
}

/// Walk a transaction extra field (padding 0x00, pubkey 0x01, nonce 0x02, additional
/// pubkeys 0x04); stops at the first unknown tag.
pub fn parse_extra(extra: &[u8]) -> Extra {
    let mut out = Extra::default();
    let mut i = 0;
    while i < extra.len() {
        match extra[i] {
            0x00 => i += 1,
            0x01 => {
                let Some(k) = extra.get(i + 1..i + 33) else { break };
                if out.tx_pubkey.is_none() {
                    out.tx_pubkey = k.try_into().ok();
                }
                i += 33;
            }
            t @ (0x02 | 0x04) => {
                let Some((n, l)) = varint_at(&extra[i + 1..]) else { break };
                let len = if t == 0x02 { n } else { n * 32 };
                let Some(d) = extra.get(i + 1 + l..i + 1 + l + len) else { break };
                if t == 0x02 {
                    out.nonces.push(d.to_vec());
                }
                i += 1 + l + len;
            }
            _ => break,
        }
    }
    out
}

/// A coinbase output: amount, one-time key, view tag.
pub type Out = (u64, [u8; 32], Option<u8>);

#[derive(Debug, Clone, PartialEq)]
pub struct Verified {
    pub name: String,
    pub address: String,
    /// Sum of the coinbase outputs that pay the tagged address.
    pub reward: u64,
    pub outputs: Vec<usize>,
}

/// The whole check for one block: find a tag in the coinbase extra, check the view key
/// against the address (A = a*G) and that at least one coinbase output pays the address.
/// None when there is no tag or it does not verify.
pub fn verify_coinbase(extra: &[u8], outs: &[Out]) -> Option<Verified> {
    let ex = parse_extra(extra);
    let r: EdwardsPoint = CompressedEdwardsY(ex.tx_pubkey?).decompress()?;
    let tag = ex.nonces.iter().find_map(|n| Tag::decode(n))?;
    let vk = parse_view_key(&hex::encode(tag.view_secret)).ok()?;
    let spend = CompressedEdwardsY(tag.spend).decompress()?;
    let address = tag.address()?; // A = a*G by construction
    let (mut reward, mut outputs) = (0u64, vec![]);
    for (i, (amount, key, vt)) in outs.iter().enumerate() {
        if output_is_ours(&vk, &spend, &r, i as u64, key, *vt) {
            reward += amount;
            outputs.push(i);
        }
    }
    (!outputs.is_empty()).then_some(Verified { name: tag.name, address, reward, outputs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use curve25519_dalek::constants::ED25519_BASEPOINT_POINT as G;
    use curve25519_dalek::scalar::Scalar;

    // A real participant of the live chain (see crypto.rs tests) and two captured coinbases.
    const ADDR: &str = "SM64knoWCgJGhSoy6ZhMdqgwMSZ4Xo8sgdTi24Q33GNS9xvaPDs9wNEeaLK3bxw5jJTNkoEsvfLYKU49jE98PU6W22JyReTqjE";
    const VK: &str = "5d4c73aff0c75911e15f488bd1f300e5839659dfcbe2ec9befb57731484e1a02";
    const R1: &str = "04689dd699b6db70c65648a42562049646867526e0ddf97cc7141e1935768cb3";
    const K1: &str = "3312a04f085770a394c96f1411d51c91aab7735be02453f283fc718ed4a87d96";
    const R2: &str = "96f39658e54ec0bb6a9b8da48d57141d94e23b0bfecd9f22ea0eb9e376c4e7f6";

    fn extra_with(r: &str, nonce: &[u8]) -> Vec<u8> {
        let mut e = vec![0x01];
        e.extend(hex::decode(r).unwrap());
        e.push(0x02);
        e.extend(varint(nonce.len() as u64));
        e.extend(nonce);
        e
    }
    fn k(h: &str) -> [u8; 32] {
        hex::decode(h).unwrap().try_into().unwrap()
    }

    #[test]
    fn codec_roundtrip_and_limits() {
        let t = Tag::new("  Alice ", ADDR, VK).unwrap();
        assert_eq!(t.name, "Alice");
        let b = t.encode();
        assert_eq!(&b[..7], b"SM64LB1");
        assert_eq!(b.len(), 7 + 1 + 5 + 64);
        assert_eq!(Tag::decode(&b).unwrap(), t);
        assert_eq!(t.address().unwrap(), ADDR);
        // longest name: 96 bytes in total
        let long = Tag::new(&"é".repeat(12), ADDR, VK).unwrap();
        assert_eq!(long.encode().len(), 96);
        assert!(Tag::new(&"a".repeat(25), ADDR, VK).is_err());
        assert!(Tag::new("", ADDR, VK).is_err());
        assert!(Tag::new("a\nb", ADDR, VK).is_err());
        // wrong view key for the address
        assert!(Tag::new("x", ADDR, &"01".to_string().repeat(1).chars().chain("00".repeat(31).chars()).collect::<String>()).is_err());
        // damaged records
        assert!(Tag::decode(&b[..b.len() - 1]).is_none());
        let mut bad = b.clone();
        bad[0] = b'X';
        assert!(Tag::decode(&bad).is_none());
        let mut bad = b.clone();
        bad[7] = 0;
        assert!(Tag::decode(&bad).is_none());
        let mut bad = b.clone();
        bad[8] = 0xff; // invalid UTF-8
        assert!(Tag::decode(&bad).is_none());
    }

    #[test]
    fn extra_parsing() {
        let mut e = vec![0x00, 0x00];
        e.extend(extra_with(R1, b"hello"));
        let p = parse_extra(&e);
        assert_eq!(p.tx_pubkey, Some(k(R1)));
        assert_eq!(p.nonces, vec![b"hello".to_vec()]);
        // a nonce longer than 127 bytes has a two-byte length
        let p = parse_extra(&extra_with(R1, &[7u8; 200]));
        assert_eq!(p.nonces[0].len(), 200);
        assert!(parse_extra(&[0x01, 1, 2]).tx_pubkey.is_none());
    }

    #[test]
    fn real_blocks_verify() {
        let tag = Tag::new("Knowc", ADDR, VK).unwrap().encode();
        let outs1 = [(35_000_000_000_000u64, k(K1), Some(0x56u8))];
        let v = verify_coinbase(&extra_with(R1, &tag), &outs1).unwrap();
        assert_eq!((v.name.as_str(), v.address.as_str(), v.reward, v.outputs.clone()), ("Knowc", ADDR, 35_000_000_000_000, vec![0]));
        // no view tag recorded: still verifies
        assert!(verify_coinbase(&extra_with(R1, &tag), &[(1, k(K1), None)]).is_some());
        // untagged coinbase
        assert!(verify_coinbase(&extra_with(R1, b"nothing"), &outs1).is_none());
        // forged: someone copies the tag into a block of their own (here: block 2's tx key with block 1's output)
        assert!(verify_coinbase(&extra_with(R2, &tag), &outs1).is_none());
        // forged: right output but a different address's tag (view key matches that other address, which is not paid)
        let sk = Scalar::from(12345u64);
        let other_addr = encode_address(&Address { prefix: PREFIX_STANDARD, spend: (sk * G).compress().to_bytes(), view: (Scalar::from(777u64) * G).compress().to_bytes() });
        let other = Tag::new("Mallory", &other_addr, &hex::encode(Scalar::from(777u64).to_bytes())).unwrap().encode();
        assert!(verify_coinbase(&extra_with(R1, &other), &outs1).is_none());
        // forged: right address, wrong view key (record claims a key that does not match)
        let mut wrong = Tag::decode(&tag).unwrap();
        wrong.view_secret = Scalar::from(5u64).to_bytes();
        let mut w = wrong.encode();
        assert!(verify_coinbase(&extra_with(R1, &w), &outs1).is_none());
        // forged: spend key swapped, view key kept
        w = Tag::decode(&tag).unwrap().encode();
        w[8 + 5..8 + 5 + 32].copy_from_slice(&(Scalar::from(3u64) * G).compress().to_bytes());
        assert!(verify_coinbase(&extra_with(R1, &w), &outs1).is_none());
    }
}
