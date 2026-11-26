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

const rom = {
  status: async () => (await cmd('host_info')).rom,
  set: (path) => cmd('rom_set', { path }),
};

export const tauri = {
  name: 'tauri',
  desktop: true,
  /** Tauri mobile (Android APK, later): same commands, but no local node controls; see desktop/README.md. */
  mobile: /Android|iPhone|iPad/.test(globalThis.navigator ? navigator.userAgent : ''),
  /** JSON-RPC to the local node: resolves with the whole `{result}` / `{error}` answer (whitelisted methods only). */
  rpc: (method, params) => cmd('node_rpc', { method, params: params || {} }),
  nodeUrl: () => nodeRpcUrl,
  node,
  nativeMiner,
  rom,
  host: () => cmd('host_info'),
};
