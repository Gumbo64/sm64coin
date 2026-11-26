//! A fresh payout subaddress per template (v0.4, docs/V0.4-DESIGN.md section 3.2, decision 13): a
//! proof record links its run to the payout address its template commits, so a miner that holds
//! its wallet's secret view key commits subaddress (0, k) for each template, k from a counter kept
//! next to the signed file. The coinbase miner output still pays the main address.
//!
//! Monero base58 and the address layout (prefix varint, spend, view, 4-byte keccak checksum) are
//! implemented here; the subaddress derivation is `sm64pow::payout::subaddress`.

use anyhow::{Result, bail};
use std::path::PathBuf;

const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const SIZES: [usize; 9] = [0, 2, 3, 5, 6, 7, 9, 10, 11];

pub fn b58_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for blk in data.chunks(8) {
        let mut n = blk.iter().fold(0u64, |a, &b| a << 8 | b as u64);
        let mut s = vec![b'1'; SIZES[blk.len()]];
        for c in s.iter_mut().rev() {
            *c = ALPHABET[(n % 58) as usize];
            n /= 58;
        }
        out.push_str(std::str::from_utf8(&s).unwrap());
    }
    out
}

pub fn b58_decode(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for blk in s.as_bytes().chunks(11) {
        let Some(size) = SIZES.iter().position(|&x| x == blk.len()) else { bail!("bad base58 length") };
        let mut n: u128 = 0;
        for &c in blk {
            let Some(d) = ALPHABET.iter().position(|&a| a == c) else { bail!("bad base58 character") };
            n = n * 58 + d as u128;
        }
        if size < 8 && n >> (8 * size) != 0 || n > u64::MAX as u128 {
            bail!("base58 block overflows");
        }
        out.extend_from_slice(&(n as u64).to_be_bytes()[8 - size..]);
    }
    Ok(out)
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// A decoded standard or sub address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    pub prefix: u64,
    pub spend: [u8; 32],
    pub view: [u8; 32],
}

pub fn decode(addr: &str) -> Result<Address> {
    let b = b58_decode(addr)?;
    if b.len() < 4 + 64 + 1 {
        bail!("address too short");
    }
    let (body, sum) = b.split_at(b.len() - 4);
    if sm64pow::sign::cn_fast_hash(body)[..4] != *sum {
        bail!("address checksum mismatch");
    }
    let (mut prefix, mut shift, mut i) = (0u64, 0, 0);
    loop {
        let Some(&c) = body.get(i) else { bail!("bad prefix") };
        prefix |= ((c & 0x7f) as u64) << shift;
        i += 1;
        shift += 7;
        if c & 0x80 == 0 {
            break;
        }
        if shift > 63 {
            bail!("bad prefix");
        }
    }
    if body.len() != i + 64 {
        bail!("bad address length");
    }
    Ok(Address { prefix, spend: body[i..i + 32].try_into().unwrap(), view: body[i + 32..].try_into().unwrap() })
}

pub fn encode(a: &Address) -> String {
    let mut d = Vec::new();
    varint(&mut d, a.prefix);
    d.extend_from_slice(&a.spend);
    d.extend_from_slice(&a.view);
    let sum = sm64pow::sign::cn_fast_hash(&d);
    d.extend_from_slice(&sum[..4]);
    b58_encode(&d)
}

/// The subaddress prefix of a network, from its standard address prefix (cryptonote_config.h:
/// mainnet 705687 / 2245783, testnet 53 / 63, stagenet 24 / 36).
pub fn subaddress_prefix(main: u64) -> Option<u64> {
    match main {
        705687 => Some(2245783),
        53 => Some(63),
        24 => Some(36),
        _ => None,
    }
}

/// Hands out subaddresses (0, k) of one wallet.
pub struct Payouts {
    view_secret: [u8; 32],
    main: Address,
    counter: PathBuf,
}

impl Payouts {
    /// `main_address`: the wallet's main address; `view_secret_hex`: its secret view key (checked
    /// against the address); the counter lives in `<signed_file>.payout`.
    pub fn new(main_address: &str, view_secret_hex: &str, signed_file: &str) -> Result<Payouts> {
        let main = decode(main_address)?;
        if subaddress_prefix(main.prefix).is_none() {
            bail!("--address is not a standard SM64Coin address (unknown prefix {})", main.prefix);
        }
        let Ok(view_secret) = <[u8; 32]>::try_from(hex::decode(view_secret_hex.trim())?.as_slice()) else { bail!("the view secret is not 32 bytes") };
        // the secret must belong to the address: a * G == A
        let (_, a_pub) = sm64pow::payout::subaddress(&view_secret, &main.spend, 0, 0).ok_or_else(|| anyhow::anyhow!("the view secret is not a valid key"))?;
        if a_pub != main.view {
            bail!("--payout-view-secret does not match --address");
        }
        Ok(Payouts { view_secret, main, counter: PathBuf::from(format!("{signed_file}.payout")) })
    }

    /// The subaddress (0, k) as an address string; k starts at 1 ((0, 0) is the main address) and
    /// is persisted before it is used, so a restart never reuses one.
    pub fn next(&self) -> Result<String> {
        let k = std::fs::read_to_string(&self.counter).ok().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0).checked_add(1).ok_or_else(|| anyhow::anyhow!("subaddress counter overflow"))?;
        std::fs::write(&self.counter, k.to_string())?;
        self.address(k)
    }

    pub fn address(&self, minor: u32) -> Result<String> {
        let (d, c) = sm64pow::payout::subaddress(&self.view_secret, &self.main.spend, 0, minor).ok_or_else(|| anyhow::anyhow!("subaddress derivation failed"))?;
        Ok(encode(&Address { prefix: subaddress_prefix(self.main.prefix).unwrap(), spend: d, view: c }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base58_round_trips_and_addresses_have_monero_lengths() {
        for n in 0..40 {
            let d: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(b58_decode(&b58_encode(&d)).unwrap(), d);
        }
        // A well-known Monero mainnet address (the Monero project's donation address) decodes with
        // a good checksum, prefix 18 and re-encodes identically.
        let donate = "44AFFq5kSiGBoZ4NMDwYtN18obc8AemS33DBLWs3H7otXft3XjrpDtQGv7SqSsaBYBb98uNbr2VBBEt7f2wfn3RVGQBEP3A";
        let a = decode(donate).unwrap();
        assert_eq!(a.prefix, 18);
        assert_eq!(encode(&a), donate);
        assert!(decode(&donate.replace("44AFF", "44AFG")).is_err());
    }

    #[test]
    fn subaddresses_per_template_persist_and_differ() {
        // A wallet: view secret a (a small canonical scalar), spend key B, main address on testnet (prefix 53).
        let mut a = [0u8; 32];
        a[0] = 7;
        let main_view = sm64pow::payout::subaddress(&a, &sm64pow::sign::Keypair::from_entropy(&[3; 32]).unwrap().public, 0, 0).unwrap().1;
        let spend = sm64pow::sign::Keypair::from_entropy(&[3; 32]).unwrap().public;
        let main = encode(&Address { prefix: 53, spend, view: main_view });
        let dir = std::env::temp_dir().join(format!("sm64-payout-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let signed = dir.join("signed.json").to_string_lossy().to_string();
        let _ = std::fs::remove_file(format!("{signed}.payout"));
        assert!(Payouts::new(&main, &hex::encode([8u8; 32]), &signed).is_err(), "a view secret of another wallet");
        let p = Payouts::new(&main, &hex::encode(a), &signed).unwrap();
        let (s1, s2) = (p.next().unwrap(), p.next().unwrap());
        assert_ne!(s1, s2);
        assert_eq!(s1, p.address(1).unwrap());
        let d = decode(&s1).unwrap();
        assert_eq!(d.prefix, 63, "testnet subaddress prefix");
        assert!(s1.starts_with('B'), "testnet subaddresses start with B: {s1}");
        // persisted: a new handle continues
        let p2 = Payouts::new(&main, &hex::encode(a), &signed).unwrap();
        assert_eq!(p2.next().unwrap(), p.address(3).unwrap());
        // main-net prefix pair
        assert_eq!(subaddress_prefix(705687), Some(2245783));
        let _ = std::fs::remove_dir_all(dir);
    }
}
