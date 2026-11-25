// v0.3 signed proofs: a throwaway miner key per block template.
//
// The key is chosen BEFORE the run is played: its extra nonce (0x64 || public key) goes into
// get_block_template, the daemon puts it in the coinbase, and the returned sm64_seed_hash (which
// drives the pokes) covers it. A different key is a different seed. The block then carries
// sm64_sig, a signature with that key over (seed, proof id, frames): see pow/sm64pow/src/sign.rs.
// The key pair and the signature come from the Rust code (pow/webminer, WASM), loaded on demand.
import { bytesToHex, hexToBytes } from './block.js';

let impl = null;
/** The WASM functions {miner_keypair(entropy) -> json, sign_proof(secret, seedHex, proof) -> hex}; tests set fakes. */
export function setSignerImpl(i) { impl = i; }

async function signer() {
  if (impl) return impl;
  const [{ default: init, miner_keypair, sign_proof }, { default: url }] = await Promise.all([
    import('../wasm/webminer/sm64coin_webminer.js'),
    import('../wasm/webminer/sm64coin_webminer_bg.wasm?url'),
  ]);
  await init({ module_or_path: url });
  impl = { miner_keypair, sign_proof };
  return impl;
}

/** A fresh key {secret_hex, public_hex, extra_nonce_hex} from 32 random bytes. */
export async function newMinerKey() {
  const s = await signer();
  const e = new Uint8Array(32);
  crypto.getRandomValues(e);
  return JSON.parse(s.miner_keypair(e));
}

/** get_block_template params: the key's extra nonce and no reserve_size (the daemon refuses both). */
export function templateParams(address, key, extra = {}) {
  return { wallet_address: address, extra_nonce: key.extra_nonce_hex, ...extra };
}

/** The block's sm64_sig (hex) for `proof` (bytes) on the template that was made with `key` (seed: its sm64_seed_hash). */
export async function signRun(key, seedHex, proof) {
  const s = await signer();
  return s.sign_proof(key.secret_hex, seedHex, proof);
}
