// The platforms of the Tauri 2 app: `tauri` (desktop: Rust does the node, the ROM and the native miner) and `tauri-mobile` (Android).
// Every Rust command answers `{code, message}` on failure; here that becomes an Error with `.code`.
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { makeBook } from '../lib/wallet/addrbook.js';

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
  /** {p2p_port, p2p_external_port, auto_start}: saved before a start. `p2p_external_port` is 0 for "same as p2p_port". */
  settings: () => cmd('node_settings'),
  /** Saves the settings; a running node keeps its ports until it is restarted. */
  setSettings: (settings) => cmd('node_settings_set', { settings }),
  /** Asks the VPN/router's gateway for the public port of `port` (default: the saved P2P port), trying NAT-PMP, PCP and
   *  UPnP-IGD. A provider with a private mechanism returns a clear error and the port is entered by hand. */
  detectPort: (port) => cmd('node_port_detect', { port: port || null }),
  /** {dir, daemon, app}: the tails of the node's daemon log and its startup log. */
  log: (lines = 200) => cmd('node_log', { lines }),
};

let netInfo = null; // the last net_status: the loopback daemon, the node and transport in use, the node list

/** The app's light-client network (Rust: lcnet.rs): `status()` -> {proxy, used: {node, transport, method}, nodes: [...],
 *  verify: {state: 'checking'|'verified'|'conflict'|'unverified', label: 'verified (3 nodes agree)', agree, quorum, height, replay}}
 *  (`verify` is the chain check of lcverify.rs; `verified()` is its label).
 *  `walletDaemon()` is the loopback daemon (http://127.0.0.1:port) the wallet engine and the native miner use: it fails over
 *  between the nodes, so a dead one is skipped. '' until `status()` has answered once. */
const net = {
  status: async () => { netInfo = await cmd('net_status'); return netInfo; },
  walletDaemon: () => (netInfo && netInfo.proxy) || '',
  verified: () => (netInfo && netInfo.verify && netInfo.verify.label) || '',
};

/** The chain tip, followed in Rust (desktop/src-tauri/src/lctip.rs): the local node polled once for everybody, or the light-client
 *  network's SubscribeTip push, whichever is available. `subscribe(fn)` calls `fn(record)` with the current tip at once (if one is
 *  known) and with each new one: {height, hash, timestamp, difficulty, cumulative_difficulty, server_time, node} (strings for the
 *  two big numbers). Returns a stop function. The page's tip store (lib/tip.js) is the only caller. */
const tip = {
  current: () => cmd('tip_get'),
  subscribe(fn) {
    let off = null, dead = false;
    listen('tip', (e) => { if (!dead) fn(e.payload); }).then((u) => { if (dead) u(); else off = u; });
    cmd('tip_get').then((j) => { if (!dead && j) fn(j); }).catch(() => {});
    return () => { dead = true; if (off) off(); };
  },
};

const nativeMiner = {
  start: ({ address, threads, viz } = {}) => cmd('miner_start', { address, threads, viz }),
  stop: () => cmd('miner_stop'),
  status: () => cmd('miner_status'),
  /** fn(line) for each output line; returns a function that stops listening. */
  subscribe(fn) {
    let off = null, dead = false;
    listen('miner-log', (e) => fn(e.payload)).then((u) => { if (dead) u(); else off = u; });
    return () => { dead = true; if (off) off(); };
  },
  /** The visualisation feed (the same data the WASM workers post, see lib/webminer/nativeFeed.js): fn({w, kind, key, data}). */
  subscribeViz(fn) {
    let off = null, dead = false;
    listen('miner-viz', (e) => fn(e.payload)).then((u) => { if (dead) u(); else off = u; });
    return () => { dead = true; if (off) off(); };
  },
  /** Switches the feed on or off (the search goes on; off it only counts frames). */
  vizOn: (on) => cmd('miner_viz_on', { on }),
  /** The page's mailbox for the searches: any of {goal, cmds, reqs} (keys replace; see rrt/vizio.rs). */
  vizMail: (mail) => cmd('miner_viz_mail', { mail }),
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

// The Android app's native miner (Rust: the miner library in the app's process, src-tauri/src/mobile_miner.rs). Same shape as the
// desktop's; the ROM is the page's own (IndexedDB), handed to Rust once with rom_put, which keeps it in the app's data.
async function pageRomBytes() {
  const f = await import('sm64-binds-src/scripts/fileUpload.js');
  const b = await f.getROM();
  return b instanceof ArrayBuffer ? new Uint8Array(b) : new Uint8Array(b.buffer || b);
}
// Android's IPC carries no raw bodies (every invoke is a JSON message; a Uint8Array would become 8 M numbers), so base64.
async function pageRomBase64() {
  const bytes = await pageRomBytes();
  const url = await new Promise((res, rej) => {
    const r = new FileReader();
    r.onload = () => res(r.result);
    r.onerror = () => rej(r.error);
    r.readAsDataURL(new Blob([bytes]));
  });
  return url.slice(url.indexOf(',') + 1);
}
// Mining only while the app is on screen (no wake lock or foreground service in this version): whichever page is open, a
// hidden app stops the miner, and a start that finishes while hidden is stopped at once.
const stopIfHidden = () => { if (globalThis.document && document.hidden) cmd('miner_stop').catch(() => {}); };
const mobileMiner = {
  ...nativeMiner,
  async start({ address, threads, viz } = {}) {
    const args = { address, threads, viz };
    let r;
    try {
      r = await cmd('miner_start', args);
    } catch (e) {
      if (e.code !== 'rom_missing') throw e;
      await cmd('rom_put', { rom: await pageRomBase64() }); // checked by size and SHA-1 in Rust
      r = await cmd('miner_start', args);
    }
    stopIfHidden();
    return r;
  },
};

const isMobile = /Android|iPhone|iPad/.test(globalThis.navigator ? navigator.userAgent : '');
// Hooked at load, so a miner still running in Rust after a page reload is stopped on hide too.
if (isMobile && globalThis.document) document.addEventListener('visibilitychange', stopIfHidden);

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
  /** {auto_lock_minutes, password_on_send, hide_balance}; weakening a protection needs `password` (or the wallet's `pin`). */
  settings: (s, password, pin) => cmd('wallet_settings', { settings: s, password, pin }),
  // Quick-unlock (optional; walletpin.rs): a 4-8 digit PIN seals the wallet password in the app's data folder (mode 0600), 5 wrong
  // tries wipe it. The password never comes back to the page. `state().pins` = [{name, left, generated}].
  /** Turns the PIN on for the open wallet. `password` is the current one ('' or absent for a wallet without one: it then gets a random
   *  password, and the answer says `generated`). */
  pinEnable: (pin, password) => cmd('wallet_pin_enable', { pin, password }),
  /** Opens wallet `name` with its PIN (rejects with code wrong_pin / pin_wiped). */
  async pinOpen(name, pin) { await cmd('wallet_pin_open', { name, pin }); return { address: (await wcall('get_address', { account_index: 0 })).address }; },
  /** Turns the PIN off (`password` or `pin` proves it is the user). For a wallet whose password the PIN made, `newPassword` ('' = none) replaces it. */
  pinDisable: ({ password, pin, newPassword } = {}) => cmd('wallet_pin_disable', { password, pin, newPassword }),
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
  /** Sets / changes the open wallet's password (`oldPassword` '' for a wallet made without one). */
  async setPassword(oldPassword, newPassword) { await wcall('change_wallet_password', { new_password: newPassword, _password: oldPassword }); },
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
  async draft({ address, amount, password, pin }) {
    const r = await wcall('transfer', { destinations: [{ address, amount: String(amount) }], account_index: 0, priority: 0, _password: password, _pin: pin });
    return { fee: r.fee, amount: r.amount, metadata: r.tx_metadata, txid: r.tx_hash };
  },
  /** Sends what `draft` built. */
  async relay(metadata) { return (await wcall('relay_tx', { hex: metadata })).tx_hash; },
  /** The seed words (and keys when asked): always needs the wallet password. */
  async seed(password, pin) { return (await wcall('query_key', { key_type: 'mnemonic', _password: password, _pin: pin })).key; },
  async keys(password, pin) {
    const [spend, view] = [await wcall('query_key', { key_type: 'spend_key', _password: password, _pin: pin }), await wcall('query_key', { key_type: 'view_key', _password: password, _pin: pin })];
    return { spendKey: spend.key, viewKey: view.key };
  },
  refresh: () => wcall('refresh'),
  /** Point the wallet at this app's node (`127.0.0.1:port`) or an https node (the public one). */
  setNode: (address) => wcall('set_daemon', { address }),
  /** The address book (wallet2's own, inside the wallet file): list/add/edit/remove with the shared rules of lib/wallet/addrbook.js. */
  book: makeBook({
    validate: async (a) => { const r = await wcall('validate_address', { address: a, any_net_type: false }); if (!r.valid) throw new Error('invalid'); },
    list: async () => ((await wcall('get_address_book', { entries: [] })).entries || []).map((e) => ({ index: e.index, address: e.address, label: e.description })),
    add: async (address, label) => (await wcall('add_address_book', { address, description: label })).index,
    edit: async (index, { address, label }) => { await wcall('edit_address_book', { index, set_address: address !== undefined, address: address || '', set_description: label !== undefined, description: label || '' }); },
    remove: async (index) => { await wcall('delete_address_book', { index }); },
  }),
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
  net,
  tip,
  node,
  nativeMiner,
  rom,
  wallet,
  biometric: null,
  link,
  host: () => cmd('host_info'),
};

/** The Android app (Tauri mobile): a remote node only (the public one, over https from Rust), no node controls, no sidecars,
 *  the ROM through the page's own upload. The native miner (preview) runs in the app's process; off by default. */
const mobile = {
  ...desktop,
  name: 'tauri-mobile',
  desktop: false,
  mobile: true,
  rpc: null, // no local node: lib/rpc.js uses the public node through remoteRpc
  nodeUrl: () => '',
  node: null,
  nativeMiner: mobileMiner, // battery-hungry: off by default, stopped when the app leaves the foreground (NativeControls.jsx)
  rom: null,
  remoteNode: { get: desktop.remoteNode.get, set: null }, // the public node only (no native confirmation dialog there)
  wallet: null, // the browser's WASM wallet engine (Wallet page) serves there
  // Quick-unlock with the phone's fingerprint / screen lock (src/quick.rs + QuickUnlockPlugin.kt): a hardware Keystore key that needs the
  // user's authentication for every use seals the wallet password; the page keeps only the sealed blob. Rejects with codes
  // cancelled / lockout / invalidated / unavailable.
  biometric: {
    /** {available, reason?} */
    status: () => cmd('quick_status'),
    /** Asks the user to authenticate, resolves the sealed blob (base64) of `secret`. */
    wrap: async (secret) => (await cmd('quick_wrap', { secret })).blob,
    /** Asks the user to authenticate, resolves the secret of a blob. */
    unwrap: async (blob) => (await cmd('quick_unwrap', { blob })).secret,
    forget: () => cmd('quick_forget'),
  },
  // The passkey hand-off works there too: `take(address)` returns {seed, height, address} for the page's WASM wallet.
  link: { ...link, take: (address) => cmd('link_take', { address }), finish: null },
};

export const tauri = isMobile ? mobile : desktop;
