// Moving from the legacy origin to the dedicated one (docs/WEB-SECURITY.md, option A). Nothing travels between the origins by
// itself: the user downloads files here and picks them there. Secrets are never put in a URL or sent with postMessage.
//
//   wallet: the open wallet is re-created from its spend key under a NEW password and written as the usual backup JSON
//           (engine.makeBackup / parseBackup), so a passkey wallet (whose password the user does not know) moves too.
//   miner:  `sm64coin.signedSeeds` (seed -> {print, frames, at}): the memory that stops a second, different run being signed
//           for one template. Not secret. Mining on both origins with the same address would double-sign: stop first.

export const MINER_STATE_FORMAT = 'sm64coin-miner-state';
const SIGNED = 'sm64coin.signedSeeds';
const ADDRESS = 'sm64coin.address';

/**
 * Re-encrypts the open wallet `w` with `newPassword`. `eng` is lib/wallet/engine.js (passed in so tests can fake it).
 * Returns { name, doc, address, seed, viewOnly }; `doc` is the backup JSON text. Throws if the re-created wallet is not the same wallet.
 */
export async function reencryptForMove(eng, w, newPassword) {
  if (!newPassword || newPassword.length < 8) throw new Error('The new password needs 8+ characters.');
  const keys = await eng.allKeys(w);
  const viewOnly = await eng.isViewOnly(w);
  let height = 0;
  try { height = await w.getRestoreHeight(); } catch { /* unknown: scan from 0 */ }
  let nw;
  try {
    nw = viewOnly
      ? await eng.restoreViewOnly(keys.address, keys.viewKey, newPassword, height)
      : await eng.restoreFromSpendKey(keys.spendKey, newPassword, height);
    if ((await nw.getPrimaryAddress()) !== keys.address) {
      throw new Error('This wallet cannot be re-created from its spend key (its keys are not derived from one). Use "Download backup" and keep the old password, or move the funds by sending them.');
    }
    const b = await eng.makeBackup(nw);
    return { name: b.name.replace(/\.json$/, '-moved.json'), doc: b.doc, address: keys.address, seed: keys.seed, viewOnly };
  } finally {
    if (nw) { try { await nw.close(false); } catch { /* closing */ } }
  }
}

const read = (storage, key) => { try { return storage.getItem(key); } catch { return null; } };

/** The miner's non-secret state as a JSON text. */
export function exportMinerState(storage) {
  let signed = {};
  try { signed = JSON.parse(read(storage, SIGNED) || '{}') || {}; } catch { signed = {}; }
  return JSON.stringify({ format: MINER_STATE_FORMAT, version: 1, createdAt: new Date().toISOString(), address: read(storage, ADDRESS) || '', signedSeeds: signed }, null, 2);
}

/** Text -> { signedSeeds, address }; throws a readable error for anything else. */
export function parseMinerState(text) {
  let d;
  try { d = JSON.parse(text); } catch { throw new Error('This is not a miner state file.'); }
  if (!d || d.format !== MINER_STATE_FORMAT) throw new Error('This file is not an SM64Coin miner state.');
  if (d.version > 1) throw new Error(`Miner state version ${d.version} is newer than this page understands.`);
  const out = {};
  for (const [seed, v] of Object.entries(d.signedSeeds || {})) {
    if (/^[0-9a-f]{1,128}$/i.test(seed) && v && typeof v.print === 'string' && Number.isFinite(v.frames) && Number.isFinite(v.at)) out[seed] = { print: v.print, frames: v.frames, at: v.at };
  }
  return { signedSeeds: out, address: typeof d.address === 'string' ? d.address : '' };
}

/**
 * Merges imported signed seeds into this origin's store. An entry already here wins (it is what this origin really signed);
 * the address is NOT taken (the payout address is chosen in the app, see the confirmation banner). Returns { added, kept }.
 */
export function mergeMinerState(storage, state) {
  let cur = {};
  try { cur = JSON.parse(read(storage, SIGNED) || '{}') || {}; } catch { cur = {}; }
  let added = 0, kept = 0;
  for (const [seed, v] of Object.entries(state.signedSeeds)) { if (cur[seed]) kept++; else { cur[seed] = v; added++; } }
  storage.setItem(SIGNED, JSON.stringify(cur));
  return { added, kept };
}
