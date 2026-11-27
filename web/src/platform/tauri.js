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

/** The desktop app: local node, native miner, ROM kept by the app. */
const desktop = {
  name: 'tauri',
  desktop: true,
  mobile: false,
  /** JSON-RPC to the local node: resolves with the whole `{result}` / `{error}` answer (whitelisted methods only). */
  rpc: (method, params) => cmd('node_rpc', { method, params: params || {} }),
  /** The same, against a remote node over https (the public node fallback). */
  remoteRpc: (url, method, params) => cmd('remote_rpc', { url, method, params: params || {} }),
  nodeUrl: () => nodeRpcUrl,
  node,
  nativeMiner,
  rom,
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
};

export const tauri = isMobile ? mobile : desktop;
