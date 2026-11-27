// One small platform layer, two implementations picked at startup: `browser` (a tab on the website) and `tauri` (the desktop
// app, the same UI in a webview with a Rust backend). Pages ask for what they need and treat null as "not available here":
//   platform.rpc(method, params)  JSON-RPC to the local node (desktop only; null in the browser: lib/rpc.js uses fetch)
//   platform.remoteRpc(url, method, params)  the same against the public node (or the one confirmed with remoteNode.set) over https
//   platform.remoteNode           {get, set(url)}: the https node the user confirmed in a native dialog (Network page)
//   platform.net                  {status, walletDaemon}: the app's light-client network (which node and transport is in use, the
//                                 loopback daemon the wallet and the native miner use), or null
//   platform.node                 {status, start, stop} of the local node, or null
//   platform.nativeMiner          {start, stop, status, subscribe, onState} of the native miner, or null
//   platform.wallet               the wallet-rpc sidecar's API (desktop) or null: the Wallet page then uses it, not the WASM engine
//   platform.rom                  {status, set(path), pick(), bytes()} of the ROM kept by the app, or null
// `await initPlatform()` once before the first render; `getPlatform()` after that.
import { browser } from './browser.js';

let current = browser;

/** True when the page runs inside the Tauri webview. */
export const inTauri = () => typeof window !== 'undefined' && !!window.__TAURI_INTERNALS__;

export async function initPlatform() {
  if (inTauri()) {
    try { current = (await import('./tauri.js')).tauri; } catch (e) { console.error('desktop platform failed to load', e); }
  }
  return current;
}

export const getPlatform = () => current;
