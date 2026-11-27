// What the Mine page shows first, as one pure decision (Mine.jsx drives it; walletGate.test.mjs checks it).
//   walletKnown    undefined while unknown; true when a wallet is registered on this device (opened or not: its address is stored
//                  in the clear, so a locked wallet needs no unlocking to be mined to); false when there is none
//   other          a valid address the visitor chose to mine for in this session (the interstitial's "Mine"), or ''
// Returns 'loading' | 'interstitial' (no wallet: "You don't have a wallet yet") | 'mine'.
export function mineGate({ walletKnown, other = '' }) {
  if (walletKnown === undefined) return 'loading';
  if (walletKnown) return 'mine'; // C: registered but locked is no reason to interrupt
  return other ? 'mine' : 'interstitial';
}

const KEY = 'sm64coin.mineOther';
/** The address chosen in this session's interstitial ('' when none). */
export function sessionOther(storage = (typeof sessionStorage !== 'undefined' ? sessionStorage : null)) {
  try { return storage?.getItem(KEY) || ''; } catch { return ''; }
}
export function setSessionOther(a, storage = (typeof sessionStorage !== 'undefined' ? sessionStorage : null)) {
  try { storage?.setItem(KEY, a); } catch { /* private window */ }
}
