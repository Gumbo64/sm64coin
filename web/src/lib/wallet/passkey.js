// Opt-in passkey wallet: the wallet is derived from a WebAuthn passkey's PRF output, so the same passkey
// (synced by the platform's password manager) opens the same wallet on any device, with no password and no server.
//
// Entirely client-side: the challenge is random bytes made here, the attestation/signature is never read or sent
// anywhere (there is no relying-party server), and neither the credential nor the PRF output ever leaves the
// browser. The page only talks to the node for chain data, as for any wallet.
//
// DERIVATION, version 1 (FROZEN: changing any byte below changes every passkey wallet's address; a new scheme
// must be a new version number with its own salt and user-handle version byte, never an edit of this one):
//   prf_salt   = SHA-256("sm64coin-passkey-wallet-v1")                        (the PRF `first` input)
//   prf        = the authenticator's 32-byte PRF output for that salt
//   spend_wide = HKDF-SHA256(ikm = prf, salt = empty, info = "sm64coin wallet spend v1", 64 bytes)
//   spend_key  = (spend_wide read as a little-endian integer) mod l, written as 32 little-endian bytes,
//                l = 2^252 + 27742317777372353535851937790883648493 (the ed25519 group order)  [= sc_reduce of 64 bytes]
//   password   = hex( HKDF-SHA256(ikm = prf, salt = empty, info = "sm64coin wallet password v1", 32 bytes) )
//                (the wallet2 password that encrypts the copy kept in this browser: no typed password)
//   view key, address, 25-word seed: from the spend key the standard way wallet2 does (deterministic wallet).
// The credential: rp.id = the site's host; user.id = [0x01 version][creation height, 4 bytes big-endian][16 random
// bytes]; discoverable (residentKey required), userVerification required. On another device the user handle
// returned by the passkey gives back the creation height, to start scanning there.
// WebAuthn works on https:// origins and on http://localhost (not 127.0.0.1).
import { daemon } from '../rpc.js';

const enc = new TextEncoder();
const L = (1n << 252n) + 27742317777372353535851937790883648493n;
export const SALT_LABEL = 'sm64coin-passkey-wallet-v1';

const hex = (u8) => [...u8].map((b) => b.toString(16).padStart(2, '0')).join('');

async function hkdf(prf, info, bytes) {
  const key = await crypto.subtle.importKey('raw', prf, 'HKDF', false, ['deriveBits']);
  return new Uint8Array(await crypto.subtle.deriveBits({ name: 'HKDF', hash: 'SHA-256', salt: new Uint8Array(0), info: enc.encode(info) }, key, bytes * 8));
}

/** The frozen v1 derivation: PRF output (32 bytes) -> { spendKey (hex), password (hex) }. */
export async function derive(prf) {
  const wide = await hkdf(prf, 'sm64coin wallet spend v1', 64);
  let n = 0n;
  for (let i = 63; i >= 0; i--) n = (n << 8n) | BigInt(wide[i]);
  n %= L;
  const sk = new Uint8Array(32);
  for (let i = 0; i < 32; i++) { sk[i] = Number(n & 0xffn); n >>= 8n; }
  return { spendKey: hex(sk), password: hex(await hkdf(prf, 'sm64coin wallet password v1', 32)) };
}

const salt = () => crypto.subtle.digest('SHA-256', enc.encode(SALT_LABEL));

/** Can this browser plausibly do it? (WebAuthn present; PRF is confirmed for real when a passkey is made.) */
export async function supported() {
  if (typeof window === 'undefined' || !window.PublicKeyCredential || !navigator.credentials) return false;
  if (!window.isSecureContext) return false;
  try {
    if (PublicKeyCredential.getClientCapabilities) {
      const c = await PublicKeyCredential.getClientCapabilities();
      if (c && c['extension:prf'] === false) return false;
    }
  } catch { /* unknown: try it */ }
  return true;
}

const prfFrom = (cred) => {
  const r = cred.getClientExtensionResults().prf;
  const f = r && r.results && r.results.first;
  return f ? new Uint8Array(f) : null;
};

const NO_PRF = 'This passkey or browser does not support the PRF extension, which the passkey wallet needs. Use a password wallet instead (or a browser/device with passkey PRF support: recent Chrome, Edge, Safari 18, Firefox 135+ with a platform or security key that has hmac-secret).';

/** Make a passkey and derive the wallet from it: { spendKey, password, height }. Opens no network connection but the node's for the height. */
export async function createPasskey() {
  if (!(await supported())) throw new Error(NO_PRF);
  let height = 0;
  try { height = Math.max(0, ((await daemon('get_block_count')).count || 0) - 1); } catch { /* scan from 0 */ }
  const uid = new Uint8Array(21);
  uid[0] = 1; new DataView(uid.buffer).setUint32(1, height >>> 0); crypto.getRandomValues(uid.subarray(5));
  const s = await salt();
  let cred;
  try {
    cred = await navigator.credentials.create({ publicKey: {
      rp: { name: 'SM64Coin wallet', id: location.hostname },
      user: { id: uid, name: 'SM64Coin wallet', displayName: 'SM64Coin wallet' },
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      pubKeyCredParams: [{ type: 'public-key', alg: -7 }, { type: 'public-key', alg: -257 }],
      authenticatorSelection: { residentKey: 'required', requireResidentKey: true, userVerification: 'required' },
      attestation: 'none',
      extensions: { prf: { eval: { first: s } } },
    } });
  } catch (e) { throw new Error(`Passkey was not created: ${e.message || e}`); }
  const ext = cred.getClientExtensionResults().prf;
  if (!ext || ext.enabled === false) throw new Error(NO_PRF);
  let prf = prfFrom(cred);
  if (!prf) prf = await assertPrf(cred.rawId); // some browsers only evaluate PRF on get()
  return { ...(await derive(prf)), height };
}

async function assertPrf(credId) {
  const s = await salt();
  let a;
  try {
    a = await navigator.credentials.get({ publicKey: {
      rpId: location.hostname,
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      userVerification: 'required',
      ...(credId ? { allowCredentials: [{ type: 'public-key', id: credId }] } : {}),
      extensions: { prf: { eval: { first: s } } },
    } });
  } catch (e) { throw new Error(`Passkey was not used: ${e.message || e}`); }
  const prf = prfFrom(a);
  if (!prf) throw new Error(NO_PRF);
  return prf;
}

/** Pick one of this site's passkeys (the browser's chooser) and derive: { spendKey, password, height }. */
export async function openPasskey() {
  if (!(await supported())) throw new Error(NO_PRF);
  const s = await salt();
  let a;
  try {
    a = await navigator.credentials.get({ publicKey: {
      rpId: location.hostname,
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      userVerification: 'required',
      extensions: { prf: { eval: { first: s } } },
    } });
  } catch (e) { throw new Error(`Passkey was not used: ${e.message || e}`); }
  const prf = prfFrom(a);
  if (!prf) throw new Error(NO_PRF);
  let height = 0;
  const h = a.response.userHandle && new Uint8Array(a.response.userHandle);
  if (h && h.length >= 5 && h[0] === 1) height = new DataView(h.buffer, h.byteOffset).getUint32(1);
  return { ...(await derive(prf)), height };
}
