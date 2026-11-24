// The wallet engine: monero-ts (woodser) with the SM64Coin fork's wallet2 compiled to WebAssembly
// (web/wallet-wasm, scripts/build-wallet-wasm.sh). Two static files next to the site, loaded only on
// the Wallet page: wallet/monero-ts.lib.mjs (the API, 0.7 MB) and wallet/monero.worker.js (a web
// worker that holds the wasm and all keys, 8 MB, gzips to about 3).
import { getNode, daemon } from '../rpc.js';

const BASE = import.meta.env.BASE_URL;
export const UNLOCK_WINDOW = 4; // SM64COIN: CRYPTONOTE_MINED_MONEY_UNLOCK_WINDOW (was 60)
export const ATOMIC = 1000000000000n; // 12 decimals, like Monero

let lib;
export function loadLib() {
  lib ||= (async () => {
    const worker = `${BASE}wallet/monero.worker.js`;
    const head = await fetch(worker, { method: 'HEAD' }).catch(() => null);
    if (!head || !head.ok || /html/.test(head.headers.get('content-type') || '')) {
      throw new Error('The wallet engine is not part of this build (run scripts/build-wallet-wasm.sh).');
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

export async function restoreFromSeed(seed, password, restoreHeight) {
  const m = await loadLib();
  return m.MoneroWalletFull.createWallet(cfg(m, { password, seed: seed.trim().replace(/\s+/g, ' '), restoreHeight: restoreHeight || 0 }));
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
      // blocks until the reward unlocks (a coinbase unlocks UNLOCK_WINDOW blocks after its block)
      unlockIn: t.getIsMinerTx() && h ? Math.max(0, h + UNLOCK_WINDOW - daemon) : 0,
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
