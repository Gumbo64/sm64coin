// The website's half of "Use my website passkey" (the app's half is desktop/src-tauri/src/link.rs; the protocol is described there).
// The page derives the wallet from the passkey with the existing, frozen passkey.js, then sends the wallet's seed words to the
// one-time listener the desktop app opened on 127.0.0.1, encrypted to the app's ephemeral X25519 key:
//   key = HKDF-SHA256(ikm = X25519(page private, app public), salt = app public || page public, info = INFO, 32 bytes)
//   ct  = AES-256-GCM(key, random 96-bit iv, aad = INFO, JSON {seed, height, address})
// Nothing is verified here beyond the shape of the link; the app checks the secret, the Origin and the payload.
const INFO = new TextEncoder().encode('sm64coin desktop-link v1');

export const b64u = (u8) => btoa(String.fromCharCode(...u8)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
export const unb64u = (s) => Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0));

/** `#p=<port>&s=<secret>&k=<app public key>` -> {port, secret, appKey} or null. */
export function parseLink(hash) {
  const q = new URLSearchParams((hash || '').replace(/^#/, ''));
  const port = Number(q.get('p'));
  const secret = q.get('s') || '';
  let appKey;
  try { appKey = unb64u(q.get('k') || ''); } catch { return null; }
  if (!Number.isInteger(port) || port < 1024 || port > 65535 || !/^[A-Za-z0-9_-]{16,64}$/.test(secret) || appKey.length !== 32) return null;
  return { port, secret, appKey };
}

/** Encrypts `payload` (an object) to the app's key: {pk, iv, ct} as base64url strings. */
export async function seal(appKey, payload) {
  const { subtle } = globalThis.crypto;
  const mine = await subtle.generateKey({ name: 'X25519' }, true, ['deriveBits']);
  const pk = new Uint8Array(await subtle.exportKey('raw', mine.publicKey));
  const theirs = await subtle.importKey('raw', appKey, { name: 'X25519' }, false, []);
  const shared = await subtle.deriveBits({ name: 'X25519', public: theirs }, mine.privateKey, 256);
  const salt = new Uint8Array(64); salt.set(appKey, 0); salt.set(pk, 32);
  const hk = await subtle.importKey('raw', shared, 'HKDF', false, ['deriveBits']);
  const keyBytes = await subtle.deriveBits({ name: 'HKDF', hash: 'SHA-256', salt, info: INFO }, hk, 256);
  const key = await subtle.importKey('raw', keyBytes, 'AES-GCM', false, ['encrypt']);
  const iv = globalThis.crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(await subtle.encrypt({ name: 'AES-GCM', iv, additionalData: INFO }, key, new TextEncoder().encode(JSON.stringify(payload))));
  return { pk: b64u(pk), iv: b64u(iv), ct: b64u(ct) };
}

/** POSTs the sealed payload to the app. Chrome asks the local listener first (Private Network Access preflight; the app answers it). */
export async function sendToApp({ port, secret, appKey }, payload, fetchFn = globalThis.fetch) {
  const body = JSON.stringify({ secret, ...(await seal(appKey, payload)) });
  const url = `http://127.0.0.1:${port}/link`;
  const init = { method: 'POST', headers: { 'Content-Type': 'application/json' }, body };
  let r;
  try { r = await fetchFn(url, { ...init, targetAddressSpace: 'loopback' }); } catch (e) {
    if (!(e instanceof TypeError)) throw e;
    r = await fetchFn(url, init); // a browser that does not know the option
  }
  if (!r.ok) throw new Error(`the app refused it (HTTP ${r.status}): ${await r.text().catch(() => '')}`);
}
