// The wallet PIN for the browser/Android WASM wallet: a 4-8 digit PIN wraps the wallet password (WebCrypto: PBKDF2-HMAC-SHA256,
// 600 000 rounds, then AES-256-GCM), with an attempt limit. A CONVENIENCE lock: there are only 10^4 to 10^8 PINs, so someone who
// copies this browser's storage can try them all offline; the counter only limits guessing through the app. Pure (no storage,
// no page): pin.test.mjs. The record is {v, kdf, iter, salt, iv, ct, tries, fails, generated}, base64 fields; it holds the
// sealed password and the counter, never the PIN. Nothing here logs or returns a PIN.
export const PIN_ITER = 600000;
export const PIN_TRIES = 5;
export const PIN_NOTE = "A PIN is a convenience lock; someone with this device's files could guess it. Use a password for strong protection.";

export const validPin = (p) => /^\d{4,8}$/.test(p || '');

const enc = new TextEncoder();
const dec = new TextDecoder();
const subtle = () => globalThis.crypto.subtle;
const b64 = (u8) => { let s = ''; for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode.apply(null, u8.subarray(i, i + 0x8000)); return btoa(s); };
const unb64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
const rand = (n) => globalThis.crypto.getRandomValues(new Uint8Array(n));

async function aesKey(pin, salt, iter) {
  const base = await subtle().importKey('raw', enc.encode(pin), 'PBKDF2', false, ['deriveKey']);
  return subtle().deriveKey({ name: 'PBKDF2', hash: 'SHA-256', salt, iterations: iter }, base, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
}

/** A random wallet password for a wallet that had none (48 hex characters: nothing to type or remember). */
export const randomPassword = () => Array.from(rand(24), (b) => b.toString(16).padStart(2, '0')).join('');

/** Seals `secret` under `pin` ({iter, tries} for tests). `generated`: the secret was made by the quick-unlock setup. */
export async function wrapSecret(secret, pin, { iter = PIN_ITER, tries = PIN_TRIES, generated = false } = {}) {
  if (!validPin(pin)) throw new Error('A PIN is 4 to 8 digits.');
  const salt = rand(16), iv = rand(12);
  const ct = new Uint8Array(await subtle().encrypt({ name: 'AES-GCM', iv }, await aesKey(pin, salt, iter), enc.encode(secret)));
  return { v: 1, kdf: 'pbkdf2-sha256', iter, salt: b64(salt), iv: b64(iv), ct: b64(ct), tries, fails: 0, generated: !!generated };
}

/** The sealed secret, or null for a wrong PIN or a damaged record. Does not touch the counter: see tryPin. */
export async function unwrapSecret(rec, pin) {
  try {
    if (!rec || rec.v !== 1 || rec.kdf !== 'pbkdf2-sha256' || !validPin(pin) || !(rec.iter > 0 && rec.iter <= 1e8)) return null;
    const pt = await subtle().decrypt({ name: 'AES-GCM', iv: unb64(rec.iv) }, await aesKey(pin, unb64(rec.salt), rec.iter), unb64(rec.ct));
    return dec.decode(pt);
  } catch { return null; }
}

/** Wrong tries left (0 when there is no record). */
export const triesLeft = (rec) => (rec ? Math.max(0, Math.min(100, Math.max(1, rec.tries | 0)) - (rec.fails | 0)) : 0);

/**
 * One PIN try. `save(rec | null)` persists the record (null = delete it). The failure is counted and saved BEFORE the PIN is
 * checked (a tab closed in the middle gives no free guess); a right PIN resets the count; the last wrong try deletes the record.
 * Resolves {ok: true, secret} | {ok: false, left, wiped}.
 */
export async function tryPin(rec, pin, save) {
  if (!rec) return { ok: false, left: 0, wiped: true };
  const tries = Math.min(100, Math.max(1, rec.tries | 0));
  const counted = { ...rec, fails: (rec.fails | 0) + 1 };
  await save(counted);
  const secret = await unwrapSecret(counted, pin);
  if (secret !== null) { await save({ ...counted, fails: 0 }); return { ok: true, secret }; }
  if (counted.fails >= tries) { await save(null); return { ok: false, left: 0, wiped: true }; }
  return { ok: false, left: tries - counted.fails, wiped: false };
}

/** The message for a failed try. */
export const pinFailure = (r) => (r.wiped ? 'The PIN is gone (too many wrong tries): use the wallet password (if quick unlock made it, restore the wallet from its seed words).' : `Wrong PIN: ${r.left} ${r.left === 1 ? 'try' : 'tries'} left.`);
