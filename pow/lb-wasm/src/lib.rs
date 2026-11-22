//! The explorer's side of the on-chain leaderboard: parse and verify a coinbase's tag.

use sm64lb::tag::{Out, Tag, verify_coinbase};
use wasm_bindgen::prelude::*;

/// The coinbase tag check. `keys` is the outputs' one-time keys back to back (32 bytes each),
/// `view_tags` one byte per output (or empty when the outputs carry none), `amounts` the
/// atomic amounts. Returns "" (no tag, or it does not verify) or
/// `{"name":..,"address":..,"reward":N,"outputs":[..]}`.
#[wasm_bindgen]
pub fn verify_tagged_coinbase(extra: &[u8], keys: &[u8], view_tags: &[u8], amounts: &[f64]) -> String {
    let n = keys.len() / 32;
    if amounts.len() != n || !(view_tags.is_empty() || view_tags.len() == n) {
        return String::new();
    }
    let outs: Vec<Out> = (0..n)
        .map(|i| (amounts[i] as u64, keys[i * 32..i * 32 + 32].try_into().unwrap(), view_tags.get(i).copied()))
        .collect();
    match verify_coinbase(extra, &outs) {
        Some(v) => format!(
            "{{\"name\":{},\"address\":\"{}\",\"reward\":{},\"outputs\":[{}]}}",
            json_str(&v.name),
            v.address,
            v.reward,
            v.outputs.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")
        ),
        None => String::new(),
    }
}

/// Is any extra's nonce a tag candidate (cheap pre-check before the elliptic-curve work)?
#[wasm_bindgen]
pub fn has_tag(extra: &[u8]) -> bool {
    sm64lb::tag::parse_extra(extra).nonces.iter().any(|n| Tag::decode(n).is_some())
}

/// The hex of the tag record for `address` (checks the view key belongs to it); errors say why not.
#[wasm_bindgen]
pub fn encode_tag(name: &str, address: &str, view_key_hex: &str) -> Result<String, JsError> {
    Tag::new(name, address, view_key_hex).map(|t| hex(&t.encode())).map_err(|e| JsError::new(&e))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o += "\\\"",
            '\\' => o += "\\\\",
            c if (c as u32) < 0x20 => o += &format!("\\u{:04x}", c as u32),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}
