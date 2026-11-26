// The wallet engine: monero-ts (woodser) with the SM64Coin fork's wallet2 compiled to WebAssembly
// (web/wallet-wasm, scripts/build-wallet-wasm.sh). Two static files next to the site, loaded only on
// the Wallet page: wallet/monero-ts.lib.mjs (the API, 0.7 MB) and wallet/monero.worker.js (a web
// worker that holds the wasm and all keys, 8 MB, gzips to about 3).
import { getNode, daemon } from '../rpc.js';
import { BACKUP_FORMAT, b64, parseBackup } from './backup.js';
export { BACKUP_FORMAT, parseBackup };

// v0.3 is a new chain (new genesis, block ids that also commit the miner's signature). A wallet cache
// (scanned heights, block hashes, outputs, restore height) of the v0.2 chain must never be reused on it:
// what is stored carries the chain id it was synced against, and a record of another chain is opened
// without its cache and rescanned from height 0 (the keys are kept: the seed still owns the same keys).
export const WALLET_CHAIN = 'v0.3';
let chainP;
/** `v0.3:<genesis hash>` of the node's chain (the constant alone if the node does not answer). */
export function chainId() {
  chainP ||= daemon('get_block_header_by_height', { height: 0 }).then((r) => `${WALLET_CHAIN}:${r.block_header.hash}`, () => WALLET_CHAIN);
  return chainP;
}

const BASE = import.meta.env.BASE_URL;
export const UNLOCK_WINDOW = 72; // SM64COIN: CRYPTONOTE_MINED_MONEY_UNLOCK_WINDOW (Monero 60; v0.2: 4; v0.3: 72 = 24 h of 20-minute blocks)
export const SPENDABLE_AGE = 3; // CRYPTONOTE_DEFAULT_TX_SPENDABLE_AGE (v0.3: 3 = 1 h; Monero 10): wallet2 also waits this long for any output
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
export async function makeBackup(w) {
  const { keysData } = await exportData(w);
  let restoreHeight = 0;
  try { restoreHeight = await w.getRestoreHeight(); } catch { /* unknown */ }
  const address = await w.getPrimaryAddress();
  const doc = { format: BACKUP_FORMAT, version: 1, network: 'mainnet', address, viewOnly: await w.isViewOnly(), restoreHeight, createdAt: new Date().toISOString(), keysData: b64(keysData) };
  return { name: `sm64coin-wallet-${address.slice(0, 8)}.json`, doc: JSON.stringify(doc, null, 2), keys: keysData, keysName: `sm64coin-wallet-${address.slice(0, 8)}.keys` };
}

export async function restoreViewOnly(address, viewKey, password, restoreHeight) {
  const m = await loadLib();
  return m.MoneroWalletFull.createWallet(cfg(m, { password, primaryAddress: address.trim(), privateViewKey: viewKey.trim(), restoreHeight: restoreHeight || 0 }));
}

export async function openStored(rec, password) {
  const m = await loadLib();
  const chain = await chainId();
  if (rec.chain === chain || (rec.chain && !chain.includes(':'))) {
    return m.MoneroWalletFull.openWallet(cfg(m, { password, keysData: rec.keysData, cacheData: rec.cacheData }));
  }
  // A cache of another chain (no id: saved before v0.3): keys only, restore height 0, a full rescan.
  const w = await m.MoneroWalletFull.openWallet(cfg(m, { password, keysData: rec.keysData }));
  await w.setRestoreHeight(0);
  return w;
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
/** A balance for display: at most `dp` decimals, cut (never rounded up: it never shows more than there is), thousands
 * grouped. The exact amount is fmt(). */
export const fmtShort = (a, dp = 4) => {
  if (a === undefined || a === null) return '';
  const v = BigInt(a);
  const whole = (v / ATOMIC).toLocaleString();
  const frac = (v % ATOMIC).toString().padStart(12, '0').slice(0, dp).replace(/0+$/, '');
  return frac ? `${whole}.${frac}` : whole;
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
 * renames) and, to count the address's blocks itself, is given the wallet's view key.
 */
export async function boardJoin(w, name) {
  const timestamp = Math.floor(Date.now() / 1000);
  const signature = await w.signMessage(`sm64coin-board:${timestamp}:${name}`);
  // The evidence: the heights of the blocks this wallet mined (its coinbase transfers). The node checks each
  // with one key derivation and never scans the chain; a height that is not ours refuses the whole entry.
  const heights = [...new Set((await w.getTxs())
    .filter((t) => t.getIsMinerTx() && t.getIsConfirmed() && !t.getIsFailed() && t.getHeight() > 0)
    .map((t) => t.getHeight()))].sort((a, b) => a - b).slice(-20000);
  if (!heights.length) throw new Error('This wallet has not mined a block yet (or is still syncing).');
  const address = await w.getPrimaryAddress();
  const r = await daemon('sm64_board_join', {
    name, address, view_key: await w.getPrivateViewKey(), timestamp, signature, heights,
  });
  if (r.disabled) throw new Error('The leaderboard is turned off on this node.');
  if (!r.joined && r.queued) { // its heights are verified in the background: wait for it to appear
    for (let i = 0; i < 30; i++) {
      await new Promise((ok) => setTimeout(ok, 2000));
      const b = await daemon('sm64_board', {});
      if ((b.entries || []).some((x) => x.address === address && x.name === name)) return { ...r, joined: true };
    }
    throw new Error('Submitted; the node is still verifying your blocks. Check the leaderboard in a minute.');
  }
  if (!r.joined) throw new Error(`Not on the board: ${r.reason}`);
  return r;
}
