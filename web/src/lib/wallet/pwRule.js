// The wallet password rule for a NEW password (browser/mobile WASM wallet and the desktop wallet-rpc wallet):
// empty is allowed (with a warning), otherwise 8+ characters, repeated identically. Pure: pwRule.test.mjs.
export const MIN_PASSWORD = 8;
export const NO_PASSWORD_WARNING = 'No password: anyone using this device can spend from it.';

/** {ok, error, warn} for a new password `pw` and its repetition `pw2` (what the inputs hold; shown inline, never silent). */
export function newPassword(pw, pw2) {
  if (!pw && !pw2) return { ok: true, error: '', warn: NO_PASSWORD_WARNING };
  if (pw.length < MIN_PASSWORD) return { ok: false, error: `Password too short: ${MIN_PASSWORD}+ characters, or leave it empty.`, warn: '' };
  if (pw !== pw2) return { ok: false, error: 'The two passwords differ.', warn: '' };
  return { ok: true, error: '', warn: '' };
}
