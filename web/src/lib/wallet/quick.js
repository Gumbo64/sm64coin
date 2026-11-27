// The one-click wallet: made at once with NO password (one can be set later from the Wallet page), kept in this device's store.
import { loadStored, saveStored } from './store.js';
import * as eng from './engine.js';
import { getPlatform } from '../../platform/index.js';
import { miner } from '../webminer/controller.js';

/** The desktop wallet-rpc wallets that are made without a password (their names), remembered here: they open by themselves. */
const NOPW = 'sm64coin.nopw.wallets';
const read = () => { try { return JSON.parse(localStorage.getItem(NOPW) || '[]'); } catch { return []; } };
export const isNoPw = (name) => read().includes(name);
export const setNoPw = (name, on) => {
  const l = read().filter((n) => n !== name);
  if (on) l.push(name);
  try { localStorage.setItem(NOPW, JSON.stringify(l)); } catch { /* private window */ }
};

/** Whether this device has a wallet (opened or locked): undefined is never returned; errors count as none. */
export async function hasWallet() {
  try {
    const p = getPlatform();
    if (p.wallet) { const st = await p.wallet.state(); if (st && st.wallets && st.wallets.length) return true; }
    else {
      const r = await loadStored();
      if (r) { if (r.address) miner.noteWallet(r.address); return true; }
    }
  } catch { /* none */ }
  return false;
}

/** Creates a passwordless wallet in place (website, Android: the WASM wallet, stored; desktop: wallet-rpc "main"). Resolves its address. */
export async function createQuick() {
  const p = getPlatform();
  let addr;
  if (p.wallet) {
    const st = await p.wallet.state();
    const name = (st.wallets || []).includes('main') ? `wallet-${Date.now() % 100000}` : 'main';
    addr = (await p.wallet.create({ name, password: '' })).address;
    setNoPw(name, true);
  } else {
    const w = await eng.createNew('');
    try {
      addr = await w.getPrimaryAddress();
      await saveStored({ ...(await eng.exportData(w)), chain: await eng.chainId(), address: addr, viewOnly: false, passkey: false, nopw: true });
    } finally { try { await w.close(false); } catch { /* closing */ } }
  }
  miner.noteWallet(addr, true);
  return addr;
}
