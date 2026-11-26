// JSON-RPC to the SM64Coin daemon.
// Daemon: the node URL is read from localStorage 'sm64coin.node'. Empty (the
// default) means same-origin '/json_rpc' (the Vite dev/preview proxy, see
// vite.config.js). A URL like https://node.example.org:16481 is called
// directly from the browser, which needs the node's CORS option
// (--rpc-access-control-origins, see scripts/public-node.sh).
// Wallet RPC always goes through the proxy.
import { probeChain, chainNote } from './nodechain.js';
import { getPlatform } from '../platform/index.js';
const KEY = 'sm64coin.node';

// A build for a static host (GitHub Pages) has no proxy: VITE_DEFAULT_NODE (e.g. the
// visitor's own node, http://127.0.0.1:16481) is the node until one is chosen.
const DEFAULT_NODE = (import.meta.env.VITE_DEFAULT_NODE || '').replace(/\/+$/, '');
// The public node: public-node.json next to the site ({"url": "https://..."}, e.g. a Cloudflare
// tunnel to a node's restricted RPC), read at load so it can change without a rebuild. Used for
// a visit when no node was chosen and the default (your own) node doesn't answer.
let publicNode = '';
let fallback = false; // this visit uses the public node
export const getPublicNode = () => publicNode;
export const usingPublicFallback = () => fallback;
let note = ''; // visible warning about the node's chain (old v0.2 node), '' for none
export const nodeNote = () => note;
const chosen = () => { try { return (localStorage.getItem(KEY) || '').trim(); } catch { return ''; } };
/** True when the node was set by hand (localStorage 'sm64coin.node'), not the build default or the public node. */
export const isChosenNode = () => !!chosen();
export const getNode = () => (chosen() || (fallback ? publicNode : DEFAULT_NODE)).replace(/\/+$/, '');

const withTimeout = (p, ms) => Promise.race([p, new Promise((_, no) => setTimeout(() => no(new Error('timeout')), ms))]);
// Decides this visit's node (main.jsx, before the first render).
export async function resolveNode() {
  try {
    const r = await withTimeout(fetch(`${import.meta.env.BASE_URL}public-node.json`, { cache: 'no-cache' }), 3000);
    if (r.ok) publicNode = String((await r.json()).url || '').trim().replace(/\/+$/, '');
  } catch { /* no public node */ }
  const pick = chosen();
  if (pick) { // a node the user chose is respected, but warned about when it is on another chain
    const probe = (await probeChain((m, p) => withTimeout(call(`${pick}/json_rpc`, m, p), 2500))).state;
    note = chainNote({ chosen: true, node: pick, probe });
    return;
  }
  if (!DEFAULT_NODE) return;
  const probe = await probeChain((m, p) => withTimeout(call(`${DEFAULT_NODE}/json_rpc`, m, p), 2500));
  if (probe.state === 'ok') return;
  if (publicNode) {
    fallback = true;
    note = chainNote({ fallbackFrom: probe.state, publicNode });
  } else if (probe.state === 'wrong-chain') {
    note = chainNote({ chosen: true, node: DEFAULT_NODE, probe: probe.state });
  }
}
export const setNode = (url) => {
  try {
    const v = (url || '').trim().replace(/\/+$/, '');
    if (v) localStorage.setItem(KEY, v); else localStorage.removeItem(KEY);
  } catch { /* storage unavailable */ }
};

async function call(url, method, params = {}) {
  // Desktop app: the same-origin node is the local one, reached through the Rust backend (no HTTP, no CORS, whitelisted methods).
  const native = url === '/json_rpc' ? getPlatform().rpc : null;
  if (native) {
    const j = await withTimeout(native(method, params), 60000);
    if (j.error) throw new Error(`${method}: ${j.error.message}`);
    return j.result;
  }
  // A node that never answers must not hang the page: abort after 60 s (the timer covers the body too).
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), 60000);
  let j;
  try {
    const r = await fetch(url, {
      method: 'POST',
      // text/plain keeps the POST a CORS "simple request": no OPTIONS preflight before every call to another origin's node
      // (monerod reads the JSON body whatever the content type).
      headers: { 'Content-Type': 'text/plain;charset=UTF-8' },
      body: JSON.stringify({ jsonrpc: '2.0', id: 0, method, params }),
      signal: ctl.signal,
    });
    if (!r.ok) throw new Error(`${method}: HTTP ${r.status}`);
    const len = Number(r.headers.get('content-length') || 0);
    if (len > 64 << 20) throw new Error(`${method}: response too large`);
    j = await r.json();
  } finally {
    clearTimeout(timer);
  }
  if (j.error) throw new Error(`${method}: ${j.error.message}`);
  return j.result;
}

// Same as daemon() but against an explicit node URL ('' = same-origin proxy).
export const daemonAt = (node, method, params) => call(`${node || ''}/json_rpc`, method, params);
// get_info is polled by several parts of a page at once (the chain watch, the miner, the cycle timeline): calls within
// INFO_TTL ms of each other (and concurrent ones) share one request.
const INFO_TTL = 1500;
let infoShared = null; // {node, at, p}
const sharedInfo = (node) => {
  const now = Date.now();
  if (infoShared && infoShared.node === node && now - infoShared.at < INFO_TTL) return infoShared.p;
  const p = daemonAt(node, 'get_info');
  infoShared = { node, at: now, p };
  p.catch(() => { if (infoShared && infoShared.p === p) infoShared = null; });
  return p;
};
export const daemon = (method, params) => {
  const node = getNode();
  return method === 'get_info' && (params === undefined || (params && Object.keys(params).length === 0)) ? sharedInfo(node) : daemonAt(node, method, params);
};
