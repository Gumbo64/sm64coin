// Quick-unlock for the WASM wallet (browser and Android): the pure parts the Wallet page uses (quickunlock.test.mjs).
//   record in IndexedDB (store.js loadQuick/saveQuick): {kind: 'pin', ...pin.js record}  or  {kind: 'android', blob, generated}
//   `generated`: the wallet had no password and the setup gave it a random one (so the user does not know it).
// Off by default; nothing here runs unless the user turned it on from the wallet's menu.
import { tryPin, pinFailure, validPin } from './pin.js';

/** The message for a failed biometric / Keystore call (`e.code` from the Rust bridge), or '' when the user just cancelled. */
export function bioMessage(e) {
  switch (e && e.code) {
    case 'cancelled': return '';
    case 'lockout': return 'Too many wrong fingerprints: wait a bit, or use the password.';
    case 'invalidated': return 'Your fingerprints or screen lock changed, so quick unlock was turned off. Use the password.';
    case 'unavailable': return 'No fingerprint or screen lock is set up on this phone.';
    default: return `Quick unlock failed${e && e.message ? `: ${e.message}` : ''}.`;
  }
}

/**
 * The wallet password behind what the user gave, for a gate (show seed and keys, ...) or an unlock:
 *  - `input` is the password itself when `check(input)` accepts it (it may throw: wrong password);
 *  - else, with a PIN set and a PIN-shaped input, the PIN's sealed password (a counted try; the sealed one is checked too);
 *  - with no `input` and a fingerprint set (`bio`), the password after the phone's prompt.
 * `setQuick(rec | null)` persists the record (the attempt counter). Returns the password; throws an Error with a message for people
 * (a cancelled prompt throws an Error with `.cancelled`).
 */
export async function secretFrom({ input = '', quick, setQuick, bio, check }) {
  if (!input && quick && quick.kind === 'android' && bio) {
    let secret;
    try { secret = await bio.unwrap(quick.blob); } catch (e) {
      if (e && e.code === 'invalidated') await dropQuick(quick, setQuick, bio);
      const m = bioMessage(e);
      const err = new Error(m || 'Cancelled.');
      if (!m) err.cancelled = true;
      throw err;
    }
    try { await check(secret); } catch { throw new Error('Quick unlock no longer matches this wallet: use the password.'); }
    return secret;
  }
  try { await check(input); return input; } catch (e) {
    if (quick && quick.kind === 'pin' && validPin(input)) {
      const r = await tryPin(quick, input, setQuick);
      if (!r.ok) throw new Error(pinFailure(r));
      try { await check(r.secret); } catch { throw new Error('The PIN no longer matches this wallet: use the password.'); }
      return r.secret;
    }
    throw e;
  }
}

/** Removes quick-unlock (record and Keystore key). */
export async function dropQuick(quick, setQuick, bio) {
  if (quick && quick.kind === 'android' && bio) { try { await bio.forget(); } catch { /* already gone */ } }
  await setQuick(null);
}

const GEN = (a) => `sm64coin.pwgen.${a}`;
/** A display-only hint: this wallet's password was made by the quick-unlock setup (used to explain what a wiped PIN means). */
export const generatedHint = (a) => { try { return !!localStorage.getItem(GEN(a)); } catch { return false; } };
export const setGeneratedHint = (a, on) => { try { if (on) localStorage.setItem(GEN(a), '1'); else localStorage.removeItem(GEN(a)); } catch { /* private window */ } };
