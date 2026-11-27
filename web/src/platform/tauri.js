// The platforms of the Tauri 2 app: `tauri` (desktop: Rust does the node, the ROM and the native miner) and `tauri-mobile` (Android).
// Every Rust command answers `{code, message}` on failure; here that becomes an Error with `.code`.
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

async function cmd(name, args) {
  try {
    return await invoke(name, args);
  } catch (e) {
    const err = new Error(e && e.message ? e.message : String(e));
    err.code = (e && e.code) || 'internal';
    throw err;
  }
}

let nodeRpcUrl = ''; // the local node's RPC address once it runs (the wallet engine talks to it directly)

const node = {
  status: async () => {
    const s = await cmd('node_status');
    nodeRpcUrl = s.answering && s.rpc_url ? s.rpc_url : nodeRpcUrl;
    return s;
  },
  start: () => cmd('node_start'),
  stop: () => cmd('node_stop'),
};

const nativeMiner = {
  start: ({ address, threads } = {}) => cmd('miner_start', { address, threads }),
  stop: () => cmd('miner_stop'),
  status: () => cmd('miner_status'),
  /** fn(line) for each output line; returns a function that stops listening. */
  subscribe(fn) {
    let off = null, dead = false;
    listen('miner-log', (e) => fn(e.payload)).then((u) => { if (dead) u(); else off = u; });
    return () => { dead = true; if (off) off(); };
  },
  /** fn() when the miner starts or exits. */
  onState(fn) {
    let off = null, dead = false;
    listen('miner-state', (e) => fn(e.payload)).then((u) => { if (dead) u(); else off = u; });
    return () => { dead = true; if (off) off(); };
  },
};

// Tells the page (RomGate) that the app now has a ROM, whoever asked for it.
const romChanged = () => window.dispatchEvent(new Event('sm64coin:rom'));

const rom = {
  status: async () => (await cmd('host_info')).rom,
  set: async (path) => { const r = await cmd('rom_set', { path }); romChanged(); return r; },
  /** Opens the system file dialog; resolves with the ROM info, or null when cancelled. */
  pick: async () => { const r = await cmd('rom_pick'); if (r) romChanged(); return r; },
  /** The ROM's bytes (ArrayBuffer) for the in-page game. */
  bytes: async () => { const b = await cmd('rom_bytes'); return b instanceof ArrayBuffer ? b : new Uint8Array(b).buffer; },
};

const isMobile = /Android|iPhone|iPad/.test(globalThis.navigator ? navigator.userAgent : '');

// The wallet: the sm64coin-wallet-rpc sidecar behind Rust (wallet.rs). The page never sees its port or login; Rust refuses
// methods outside its whitelist and enforces the password rules (the password goes along as `_password`). Amounts are atomic
// units as strings (BigInt(x)).
async function wcall(method, params) {
  const r = await cmd('wallet_rpc', { method, params: params || {} });
  if (r && r.error) {
    const e = new Error(r.error.message || 'wallet error');
    e.code = r.error.code;
    throw e;
  }
  return r.result;
}

const wallet = {
  /** {available, running, wallets: [names], open: name | null, settings, dir} */
  state: () => cmd('wallet_state'),
  /** Report that the user is here (restarts the idle auto-lock timer). */
  activity: () => cmd('wallet_activity').catch(() => {}),
  /** {auto_lock_minutes, password_on_send, hide_balance}; weakening a protection needs `password`. */
  settings: (s, password) => cmd('wallet_settings', { settings: s, password }),
  /** A new wallet: resolves {address, seed} (the seed is shown once, here). kdfRounds: wallet2's password hashing rounds (default 1). */
  async create({ name, password, kdfRounds = 1, language = 'English' }) {
    await wcall('create_wallet', { filename: name, password, language, _kdf_rounds: kdfRounds });
    const address = (await wcall('get_address', { account_index: 0 })).address;
    const seed = (await wcall('query_key', { key_type: 'mnemonic', _password: password })).key;
    return { address, seed };
  },
  async restore({ name, password, seed, restoreHeight = 0, seedOffset = '', kdfRounds = 1 }) {
    const r = await wcall('restore_deterministic_wallet', { filename: name, password, seed: seed.trim().replace(/\s+/g, ' '), restore_height: Number(restoreHeight) || 0, seed_offset: seedOffset || '', _kdf_rounds: kdfRounds });
    return { address: r.address };
  },
  async address() { return (await wcall('get_address', { account_index: 0 })).address; },
  async open(name, password) { await wcall('open_wallet', { filename: name, password }); return { address: (await wcall('get_address', { account_index: 0 })).address }; },
  close: () => wcall('close_wallet'),
  /** {balance, unlocked} (atomic units, strings). */
  async balance() { const r = await wcall('get_balance', { account_index: 0 }); return { balance: r.balance, unlocked: r.unlocked_balance }; },
  async height() { return (await wcall('get_height')).height; },
  /** Every transfer, newest first. */
  async transfers() {
    const r = await wcall('get_transfers', { in: true, out: true, pending: true, failed: true, pool: true, account_index: 0 });
    const rows = [...(r.in || []), ...(r.out || []), ...(r.pending || []), ...(r.failed || []), ...(r.pool || [])];
    return rows.sort((a, b) => (b.height || 1e12) - (a.height || 1e12) || (b.timestamp || 0) - (a.timestamp || 0));
  },
  /** Builds a transaction without sending it: {fee, amount, metadata}. Asks for the password when the setting is on. */
  async draft({ address, amount, password }) {
    const r = await wcall('transfer', { destinations: [{ address, amount: String(amount) }], account_index: 0, priority: 0, _password: password });
    return { fee: r.fee, amount: r.amount, metadata: r.tx_metadata, txid: r.tx_hash };
  },
  /** Sends what `draft` built. */
  async relay(metadata) { return (await wcall('relay_tx', { hex: metadata })).tx_hash; },
  /** The seed words (and keys when asked): always needs the wallet password. */
  async seed(password) { return (await wcall('query_key', { key_type: 'mnemonic', _password: password })).key; },
  async keys(password) {
    const [spend, view] = [await wcall('query_key', { key_type: 'spend_key', _password: password }), await wcall('query_key', { key_type: 'view_key', _password: password })];
    return { spendKey: spend.key, viewKey: view.key };
  },
  refresh: () => wcall('refresh'),
  /** Point the wallet at this app's node (`127.0.0.1:port`) or an https node (the public one). */
  setNode: (address) => wcall('set_daemon', { address }),
  onLocked(fn) {
    let off = null, dead = false;
    listen('wallet-locked', (e) => fn(e.payload)).then((u) => { if (dead) u(); else off = u; });
    return () => { dead = true; if (off) off(); };
  },
};

// "Use my website passkey": a one-time 127.0.0.1 listener + the system browser at the website's /desktop-link page (see link.rs).
const link = {
  /** Starts the listener and opens the browser: {state: 'waiting', url}. `open: false` only returns the URL. */
  start: (open = true) => cmd('link_start', { open }),
  /** {state: idle | waiting | received | expired | cancelled | failed, address?, error?} */
  status: () => cmd('link_status'),
  cancel: () => cmd('link_cancel'),
  /** Imports the received wallet under `name` with a password chosen in the app. `address`: the received address the user
   *  compared with the browser page (Rust also checks the restored wallet has it). */
  async finish({ name, password, address, kdfRounds = 1 }) { const r = await cmd('link_finish', { name, password, address, kdfRounds }); if (r && r.error) throw new Error(r.error.message); return r.result || r; },
};

/** The desktop app: local node, native miner, ROM kept by the app. */
const desktop = {
  name: 'tauri',
  desktop: true,
  mobile: false,
  /** JSON-RPC to the local node: resolves with the whole `{result}` / `{error}` answer (whitelisted methods only). */
  rpc: (method, params) => cmd('node_rpc', { method, params: params || {} }),
  /** The same, against a remote node over https: only the public node or the one saved with remoteNode.set (Rust checks). */
  remoteRpc: (url, method, params) => cmd('remote_rpc', { url, method, params: params || {} }),
  /** The https node the user picked on the Network page: `get()` -> {public, saved}; `set(url)` asks the user in a native
   *  dialog and resolves with the saved URL (rejects with code 'cancelled'); `set(null)` forgets it. */
  remoteNode: {
    get: () => cmd('remote_node_get'),
    set: (url) => cmd('remote_node_set', { url: url || null }),
  },
  nodeUrl: () => nodeRpcUrl,
  node,
  nativeMiner,
  rom,
  wallet,
  link,
  host: () => cmd('host_info'),
};

/** The Android app (Tauri mobile): a remote node only (the public one, over https from Rust), no node controls, no sidecars,
 *  the ROM through the page's own upload. The native miner for Android is future work (off by default, charging only). */
const mobile = {
  ...desktop,
  name: 'tauri-mobile',
  desktop: false,
  mobile: true,
  rpc: null, // no local node: lib/rpc.js uses the public node through remoteRpc
  nodeUrl: () => '',
  node: null,
  nativeMiner: null,
  rom: null,
  remoteNode: { get: desktop.remoteNode.get, set: null }, // the public node only (no native confirmation dialog there)
  wallet: null, // the browser's WASM wallet engine (Wallet page) serves there
  link: null,
};

export const tauri = isMobile ? mobile : desktop;
