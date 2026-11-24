// The wallet engine: monero-ts (woodser) with the SM64Coin fork's wallet2 compiled to WebAssembly
// (web/wallet-wasm, scripts/build-wallet-wasm.sh). Two static files next to the site, loaded only on
// the Wallet page: wallet/monero-ts.lib.mjs (the API, 0.7 MB) and wallet/monero.worker.js (a web
// worker that holds the wasm and all keys, 8 MB, gzips to about 3).
import { getNode, daemon } from '../rpc.js';

const BASE = import.meta.env.BASE_URL;
export const UNLOCK_WINDOW = 4; // SM64COIN: CRYPTONOTE_MINED_MONEY_UNLOCK_WINDOW (was 60)
export const SPENDABLE_AGE = 10; // CRYPTONOTE_DEFAULT_TX_SPENDABLE_AGE: wallet2 also waits this long for any output
export const ATOMIC = 1000000000000n; // 12 decimals, like Monero

let lib;
export function loadLib() {
  lib ||= (async () => {
    const worker = `${BASE}wallet/monero.worker.js`;
    const head = await fetch(worker, { method: 'HEAD' }).catch(() => null);
    if (!head || !head.ok || /html/.test(head.headers.get('content-type') || '')) {
      throw new Error('Wallet engine unavailable: it is not part of this build (scripts/build-wallet-wasm.sh builds it; the site\'s CI does it, and a failed build leaves it out).');
    }
    const m = await import(/* @vite-ignore */ `${location.origin}${BASE}wallet/monero-ts.lib.mjs`);
    m.LibraryUtils.setWorkerDistPath(worker);
    return m;
  })();
  lib.catch(() => { lib = undefined; });
  return lib;
}

/** The node's URL as the wallet engine (a worker) needs it: absolute. '' is this site's own proxy. */
export const daemonUri = () => getNode() || location.origin;

const net = (m) => m.MoneroNetworkType.MAINNET; // the SM64Coin chain is the fork's "mainnet" (addresses SM64...)

const cfg = (m, extra) => new m.MoneroWalletConfig({ networkType: net(m), server: { uri: daemonUri() }, proxyToWorker: true, ...extra });

async function chainHeight(w) {
  try { return await w.getDaemonHeight(); } catch { return 0; }
}

/** A new random wallet; resolves with the open wallet (its seed: getSeed()). */
export async function createNew(password) {
  const m = await loadLib();
  const w = await m.MoneroWalletFull.createWallet(cfg(m, { password, language: 'English' }));
  // wallet2 guesses a restore height from the calendar (Monero's schedule): this chain is young.
  const h = await chainHeight(w);
  await w.setRestoreHeight(Math.max(0, h - 1));
  return w;
}

// wallet2 detects the mnemonic's language from the words (monero-ts forbids passing one with a seed).
export async function restoreFromSeed(seed, password, restoreHeight, { offset } = {}) {
  const m = await loadLib();
  const extra = { password, seed: seed.trim().replace(/\s+/g, ' '), restoreHeight: restoreHeight || 0 };
  if (offset) extra.seedOffset = offset;
  return m.MoneroWalletFull.createWallet(cfg(m, extra));
}

/** A full wallet from the address and both private keys (wallet2 checks that they derive the address). */
export async function restoreFromKeys(address, viewKey, spendKey, password, restoreHeight) {
  const m = await loadLib();
  return m.MoneroWalletFull.createWallet(cfg(m, {
    password, primaryAddress: address.trim(), privateViewKey: viewKey.trim(), privateSpendKey: spendKey.trim(), restoreHeight: restoreHeight || 0,
  }));
}

/** A full wallet from just the private spend key (the view key, address and seed follow deterministically). */
export async function restoreFromSpendKey(spendKey, password, restoreHeight) {
  const m = await loadLib();
  return m.MoneroWalletFull.createWallet(cfg(m, { password, privateSpendKey: spendKey, restoreHeight: restoreHeight || 0 }));
}

/** Open raw wallet2 files (a desktop wallet's <name>.keys and optionally its cache file). */
export async function openFiles(keysData, cacheData, password) {
  const m = await loadLib();
  return m.MoneroWalletFull.openWallet(cfg(m, { password, keysData, ...(cacheData && cacheData.length ? { cacheData } : {}) }));
}

/** Throws unless `password` opens the wallet's own keys file (wallet2 checks it; a second throwaway instance is opened). */
export async function checkPassword(w, password) {
  const { keysData } = await exportData(w);
  let t;
  try { t = await openFiles(keysData, undefined, password); } catch { throw new Error('Wrong password.'); }
  try { await t.close(false); } catch { /* closing */ }
}

/** Every secret and public key of the open wallet; the seed is '' where the wallet has none (view-only / non-deterministic keys). */
export async function allKeys(w) {
  const get = async (f) => { try { return (await f()) || ''; } catch { return ''; } };
  return {
    address: await w.getPrimaryAddress(),
    seed: await get(() => w.getSeed()),
    seedLanguage: await get(() => w.getSeedLanguage()),
    viewKey: await get(() => w.getPrivateViewKey()),
    spendKey: await get(() => w.getPrivateSpendKey()),
    publicViewKey: await get(() => w.getPublicViewKey()),
    publicSpendKey: await get(() => w.getPublicSpendKey()),
  };
}

// ---- backup file: JSON around wallet2's own password-encrypted .keys bytes ----
export const BACKUP_FORMAT = 'sm64coin-wallet-backup';
const b64 = (u8) => { let s = ''; for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode.apply(null, u8.subarray(i, i + 0x8000)); return btoa(s); };
const unb64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

export async function makeBackup(w) {
  const { keysData } = await exportData(w);
  let restoreHeight = 0;
  try { restoreHeight = await w.getRestoreHeight(); } catch { /* unknown */ }
  const address = await w.getPrimaryAddress();
  const doc = { format: BACKUP_FORMAT, version: 1, network: 'mainnet', address, viewOnly: await w.isViewOnly(), restoreHeight, createdAt: new Date().toISOString(), keysData: b64(keysData) };
  return { name: `sm64coin-wallet-${address.slice(0, 8)}.json`, doc: JSON.stringify(doc, null, 2), keys: keysData, keysName: `sm64coin-wallet-${address.slice(0, 8)}.keys` };
}

/** Bytes of a picked file -> { keysData, meta? }: our JSON backup, or a raw wallet2 .keys file. */
export function parseBackup(u8) {
  if (u8[0] === 0x7b) {
    let d;
    try { d = JSON.parse(new TextDecoder().decode(u8)); } catch { throw new Error('This is not a wallet backup file.'); }
    if (d.format !== BACKUP_FORMAT) throw new Error('This JSON is not an SM64Coin wallet backup.');
    if (d.version > 1) throw new Error(`Backup format version ${d.version} is newer than this page understands.`);
    if (d.network && d.network !== 'mainnet') throw new Error(`This backup is for network "${d.network}", not SM64Coin.`);
    return { keysData: unb64(d.keysData), meta: d };
  }
  return { keysData: u8 };
}

export async function restoreViewOnly(address, viewKey, password, restoreHeight) {
  const m = await loadLib();
  return m.MoneroWalletFull.createWallet(cfg(m, { password, primaryAddress: address.trim(), privateViewKey: viewKey.trim(), restoreHeight: restoreHeight || 0 }));
}

export async function openStored(rec, password) {
  const m = await loadLib();
  return m.MoneroWalletFull.openWallet(cfg(m, { password, keysData: rec.keysData, cacheData: rec.cacheData }));
}

const bytes = (v) => new Uint8Array(v.buffer, v.byteOffset, v.byteLength).slice();
/** What to persist: the encrypted keys file and cache. */
export async function exportData(w) {
  const [keys, cache] = await w.getData();
  return { keysData: bytes(keys), cacheData: bytes(cache) };
}

export async function isViewOnly(w) { return w.isViewOnly(); }

/** One sync pass; `onProgress(percent)`. */
export async function syncOnce(w, onProgress) {
  const m = await loadLib();
  const l = new m.MoneroWalletListener();
  l.onSyncProgress = (height, start, end, pct) => onProgress && onProgress(pct, height, end);
  return w.sync(l);
}

export async function setNode(w) { return w.setDaemonConnection({ uri: daemonUri() }); }

export const fmt = (a) => {
  if (a === undefined || a === null) return '';
  const v = BigInt(a);
  const whole = v / ATOMIC, frac = (v % ATOMIC).toString().padStart(12, '0').replace(/0+$/, '');
  return frac ? `${whole}.${frac}` : `${whole}`;
};
export const parseAmount = (s) => {
  const m = /^\s*(\d+)(?:\.(\d{1,12}))?\s*$/.exec(s || '');
  return m ? BigInt(m[1]) * ATOMIC + BigInt((m[2] || '').padEnd(12, '0') || '0') : null;
};

/** Everything the page shows, from the open wallet. */
export async function snapshot(w) {
  const [balance, unlocked, height, daemon, txs] = await Promise.all([
    w.getBalance(), w.getUnlockedBalance(), w.getHeight(), chainHeight(w), w.getTxs(),
  ]);
  const rows = txs.map((t) => {
    const inc = t.getIncomingAmount();
    const out = t.getOutgoingAmount();
    const h = t.getHeight();
    return {
      hash: t.getHash(),
      height: h,
      miner: !!t.getIsMinerTx(),
      confirmed: !!t.getIsConfirmed(),
      failed: !!t.getIsFailed(),
      locked: !!t.getIsLocked(),
      incoming: inc, outgoing: out, fee: t.getFee(),
      timestamp: t.getBlock && t.getBlock() ? t.getBlock().getTimestamp() : undefined,
      // blocks until it can be spent: wallet2 (is_transfer_unlocked) needs the output's block + UNLOCK_WINDOW
      // for a coinbase and also block + SPENDABLE_AGE for every output
      unlockIn: h ? Math.max(0, h + (t.getIsMinerTx() ? Math.max(UNLOCK_WINDOW, SPENDABLE_AGE) : SPENDABLE_AGE) - daemon) : 0,
    };
  }).sort((a, b) => (b.height ?? 1e12) - (a.height ?? 1e12));
  return { balance, unlocked, height, daemon, rows };
}

/**
 * Join / rename on the leaderboard, as the desktop wallet does (gui/pages/Leaderboard.qml): the node
 * checks a signature by the wallet's spend key over "sm64coin-board:<ts>:<name>" (a later timestamp
 * renames) and, to count the address's blocks itself, is given the wallet's secret view key.
 */
export async function boardJoin(w, name) {
  const timestamp = Math.floor(Date.now() / 1000);
  const signature = await w.signMessage(`sm64coin-board:${timestamp}:${name}`);
  const r = await daemon('sm64_board_join', {
    name, address: await w.getPrimaryAddress(), view_key: await w.getPrivateViewKey(), timestamp, signature,
  });
  if (!r.joined) throw new Error(`Not on the board: ${r.reason}`);
  return r;
}
