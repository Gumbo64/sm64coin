// The on-chain leaderboard opt-in (see pow/sm64lb): a record with a display name, the
// address's public spend key and its private VIEW key goes into the coinbase extra nonce
// of the blocks this miner mines. Nodes treat it as opaque bytes; anyone can then verify
// from the chain alone that the block pays that address (pow/sm64lb, compiled to WASM).
// The record is part of the block template, so it must be there BEFORE playing (it
// changes the seed): every template this page asks for carries it.
import init, { encode_tag } from '../wasm/lb/sm64lb_wasm.js';

const KEY = 'sm64coin.lbtag';
let ready = null;
export const lbReady = () => (ready ??= init());

export const getTagSettings = () => {
  try { const v = JSON.parse(localStorage.getItem(KEY) || 'null'); return v && v.name && v.viewKey ? v : null; } catch { return null; }
};
export const setTagSettings = (v) => {
  try {
    if (v && v.name && v.viewKey) localStorage.setItem(KEY, JSON.stringify({ name: v.name.trim(), viewKey: v.viewKey.trim().toLowerCase() }));
    else localStorage.removeItem(KEY);
  } catch { /* storage unavailable */ }
};

// The wallet's "Play to mine" link may carry ?lbname=&lbkey=: take them, then drop
// the key from the address bar.
export function adoptTagFromUrl() {
  try {
    const q = new URLSearchParams(window.location.search);
    const name = q.get('lbname'), key = q.get('lbkey');
    if (name && key && /^[0-9a-fA-F]{64}$/.test(key.trim())) setTagSettings({ name, viewKey: key });
    if (q.has('lbkey')) { q.delete('lbkey'); const s = q.toString(); history.replaceState(null, '', window.location.pathname + (s ? `?${s}` : '')); }
  } catch { /* no window */ }
}

/** The tag record (hex) for `address` from the stored settings, or null (not opted in). Throws if the settings are invalid for this address. */
export async function tagHex(address) {
  const s = getTagSettings();
  if (!s) return null;
  await lbReady();
  return encode_tag(s.name, address, s.viewKey);
}

/** get_block_template params: the address, and the tag as the extra nonce when opted in. A bad tag is reported to `onError` and left out. */
export async function templateParams(address, extra = {}, onError = () => {}) {
  try {
    const t = await tagHex(address);
    if (t) return { wallet_address: address, extra_nonce: t, ...extra };
  } catch (e) { onError(String(e && e.message ? e.message : e)); }
  return { wallet_address: address, reserve_size: 0, ...extra };
}
