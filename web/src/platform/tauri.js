// The platform of the desktop app (Tauri 2): the same UI, with Rust doing the node, the ROM and the native miner.
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

export const tauri = {
  name: 'tauri',
  desktop: true,
  /** Tauri mobile (Android APK, later): same commands, but no local node controls; see desktop/README.md. */
  mobile: /Android|iPhone|iPad/.test(globalThis.navigator ? navigator.userAgent : ''),
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
