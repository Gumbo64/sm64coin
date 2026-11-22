//! Monero base58 addresses and coinbase output ownership checks.
use curve25519_dalek::constants::ED25519_BASEPOINT_POINT as G;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use sha3::{Digest, Keccak256};

pub const PREFIX_STANDARD: u64 = 705687;
pub const PREFIX_INTEGRATED: u64 = 1950871;
pub const PREFIX_SUBADDRESS: u64 = 2245783;

const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const ENC_LEN: [usize; 9] = [0, 2, 3, 5, 6, 7, 9, 10, 11];

pub fn keccak(data: &[u8]) -> [u8; 32] {
    Keccak256::digest(data).into()
}

fn b58_block_decode(s: &[u8], out_len: usize) -> Option<Vec<u8>> {
    let mut n: u128 = 0;
    for &c in s {
        let d = ALPHABET.iter().position(|&a| a == c)? as u128;
        n = n.checked_mul(58)?.checked_add(d)?;
    }
    if out_len < 16 && n >> (8 * out_len) != 0 {
        return None;
    }
    Some(n.to_be_bytes()[16 - out_len..].to_vec())
}

fn b58_block_encode(b: &[u8]) -> String {
    let mut n = b.iter().fold(0u128, |a, &x| (a << 8) | x as u128);
    let len = ENC_LEN[b.len()];
    let mut out = vec![ALPHABET[0]; len];
    for i in (0..len).rev() {
        out[i] = ALPHABET[(n % 58) as usize];
        n /= 58;
    }
    String::from_utf8(out).unwrap()
}

pub fn base58_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.as_bytes();
    let (full, rem) = (s.len() / 11, s.len() % 11);
    let mut out = Vec::new();
    for i in 0..full {
        out.extend(b58_block_decode(&s[i * 11..i * 11 + 11], 8)?);
    }
    if rem > 0 {
        let sz = ENC_LEN.iter().position(|&l| l == rem)?;
        out.extend(b58_block_decode(&s[full * 11..], sz)?);
    }
    Some(out)
}

pub fn base58_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for c in data.chunks(8) {
        out += &b58_block_encode(c);
    }
    out
}

fn read_varint(b: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for (i, &x) in b.iter().enumerate().take(10) {
        v |= ((x & 0x7f) as u64) << (7 * i);
        if x & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

pub fn varint(mut v: u64) -> Vec<u8> {
    let mut o = Vec::new();
    while v >= 0x80 {
        o.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    o.push(v as u8);
    o
}

#[derive(Debug, Clone, PartialEq)]
pub struct Address {
    pub prefix: u64,
    pub spend: [u8; 32],
    pub view: [u8; 32],
}

pub fn decode_address(s: &str) -> Result<Address, String> {
    let raw = base58_decode(s.trim()).ok_or("invalid base58")?;
    if raw.len() < 4 {
        return Err("address too short".into());
    }
    let (body, sum) = raw.split_at(raw.len() - 4);
    if keccak(body)[..4] != *sum {
        return Err("bad address checksum".into());
    }
    let (prefix, n) = read_varint(body).ok_or("bad prefix")?;
    let rest = &body[n..];
    let want = match prefix {
        PREFIX_STANDARD | PREFIX_SUBADDRESS => 64,
        PREFIX_INTEGRATED => 72,
        _ => return Err(format!("not an SM64Coin address (prefix {prefix})")),
    };
    if rest.len() != want {
        return Err("bad address length".into());
    }
    let (mut spend, mut view) = ([0u8; 32], [0u8; 32]);
    spend.copy_from_slice(&rest[..32]);
    view.copy_from_slice(&rest[32..64]);
    for k in [&spend, &view] {
        if CompressedEdwardsY(*k).decompress().is_none() {
            return Err("invalid public key in address".into());
        }
    }
    Ok(Address { prefix, spend, view })
}

pub fn encode_address(a: &Address) -> String {
    let mut b = varint(a.prefix);
    b.extend(a.spend);
    b.extend(a.view);
    let c = keccak(&b);
    b.extend(&c[..4]);
    base58_encode(&b)
}

pub fn parse_view_key(hexs: &str) -> Result<Scalar, String> {
    let v = hex::decode(hexs.trim()).map_err(|_| "view key must be hex")?;
    let arr: [u8; 32] = v.try_into().map_err(|_| "view key must be 32 bytes")?;
    Option::<Scalar>::from(Scalar::from_canonical_bytes(arr)).ok_or_else(|| "view key not a reduced scalar".to_string())
}

/// Check that the private view key matches the address's public view key.
pub fn view_key_matches(a: &Address, vk: &Scalar) -> bool {
    (vk * G).compress().to_bytes() == a.view
}

pub fn hash_to_scalar(data: &[u8]) -> Scalar {
    Scalar::from_bytes_mod_order(keccak(data))
}

/// Does coinbase output `index` (one-time key `key`, optional view tag) belong to the
/// address with public spend key `spend`, given tx pubkey `r` and private view key `a`?
pub fn output_is_ours(
    a: &Scalar,
    spend: &EdwardsPoint,
    r: &EdwardsPoint,
    index: u64,
    key: &[u8; 32],
    view_tag: Option<u8>,
) -> bool {
    let d = (a * r).mul_by_cofactor().compress().to_bytes();
    let idx = varint(index);
    if let Some(t) = view_tag {
        let mut m = b"view_tag".to_vec();
        m.extend(d);
        m.extend(&idx);
        if keccak(&m)[0] != t {
            return false;
        }
    }
    let mut m = d.to_vec();
    m.extend(&idx);
    let p = hash_to_scalar(&m) * G + spend;
    p.compress().to_bytes() == *key
}

#[cfg(test)]
mod tests {
    use super::*;
    const ADDR: &str = "SM64knoWCgJGhSoy6ZhMdqgwMSZ4Xo8sgdTi24Q33GNS9xvaPDs9wNEeaLK3bxw5jJTNkoEsvfLYKU49jE98PU6W22JyReTqjE";
    const VK: &str = "5d4c73aff0c75911e15f488bd1f300e5839659dfcbe2ec9befb57731484e1a02";
    // captured from the live chain: (tx pubkey, output key, view tag)
    const B1: (&str, &str, u8) = ("04689dd699b6db70c65648a42562049646867526e0ddf97cc7141e1935768cb3", "3312a04f085770a394c96f1411d51c91aab7735be02453f283fc718ed4a87d96", 0x56);
    const B2: (&str, &str, u8) = ("96f39658e54ec0bb6a9b8da48d57141d94e23b0bfecd9f22ea0eb9e376c4e7f6", "49bf3a48886ba0a7c3218b055a087b28c0de05a88b6893faf2ad0419ab62928b", 0xc8);

    fn pt(h: &str) -> EdwardsPoint {
        let v: [u8; 32] = hex::decode(h).unwrap().try_into().unwrap();
        CompressedEdwardsY(v).decompress().unwrap()
    }
    fn key(h: &str) -> [u8; 32] {
        hex::decode(h).unwrap().try_into().unwrap()
    }

    #[test]
    fn base58_roundtrip() {
        for n in 0..40usize {
            let d: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(base58_decode(&base58_encode(&d)).unwrap(), d);
        }
        assert_eq!(base58_encode(&[0u8; 8]), "11111111111");
        assert!(base58_decode("0OIl").is_none());
    }

    #[test]
    fn address_decode_encode() {
        let a = decode_address(ADDR).unwrap();
        assert_eq!(a.prefix, PREFIX_STANDARD);
        assert_eq!(encode_address(&a), ADDR);
        let mut bad = ADDR.to_string();
        bad.replace_range(10..11, if &ADDR[10..11] == "a" { "b" } else { "a" });
        assert!(decode_address(&bad).is_err());
    }

    #[test]
    fn view_key_check() {
        let a = decode_address(ADDR).unwrap();
        assert!(view_key_matches(&a, &parse_view_key(VK).unwrap()));
        let other = parse_view_key(&"01".repeat(1).repeat(1).chars().chain("00".repeat(31).chars()).collect::<String>()).unwrap();
        assert!(!view_key_matches(&a, &other));
    }

    #[test]
    fn coinbase_ownership_live_vectors() {
        let a = decode_address(ADDR).unwrap();
        let vk = parse_view_key(VK).unwrap();
        let spend = CompressedEdwardsY(a.spend).decompress().unwrap();
        for (r, k, t) in [B1, B2] {
            assert!(output_is_ours(&vk, &spend, &pt(r), 0, &key(k), Some(t)));
            assert!(!output_is_ours(&vk, &spend, &pt(r), 1, &key(k), Some(t)));
            assert!(!output_is_ours(&vk, &spend, &pt(r), 0, &key(k), Some(t ^ 1)));
        }
        // block 1's output is not ours under block 2's tx key
        assert!(!output_is_ours(&vk, &spend, &pt(B2.0), 0, &key(B1.1), None));
    }
}
