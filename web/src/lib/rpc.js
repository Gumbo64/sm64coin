// JSON-RPC to the SM64Coin daemon.
// Daemon: the node URL is read from localStorage 'sm64coin.node'. Empty (the
// default) means same-origin '/json_rpc' (the Vite dev/preview proxy, see
// vite.config.js). A URL like https://node.example.org:16481 is called
// directly from the browser, which needs the node's CORS option
// (--rpc-access-control-origins, see scripts/public-node.sh).
// Wallet RPC always goes through the proxy.
const KEY = 'sm64coin.node';

// A build for a static host (GitHub Pages) has no proxy: VITE_DEFAULT_NODE (e.g. the
// visitor's own node, http://127.0.0.1:16481) is the node until one is chosen.
const DEFAULT_NODE = (import.meta.env.VITE_DEFAULT_NODE || '').replace(/\/+$/, '');
export const getNode = () => {
  try { return (localStorage.getItem(KEY) || DEFAULT_NODE).trim().replace(/\/+$/, ''); } catch { return DEFAULT_NODE; }
};
export const setNode = (url) => {
  try {
    const v = (url || '').trim().replace(/\/+$/, '');
    if (v) localStorage.setItem(KEY, v); else localStorage.removeItem(KEY);
  } catch { /* storage unavailable */ }
};

async function call(url, method, params = {}) {
  const r = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 0, method, params }),
  });
  if (!r.ok) throw new Error(`${method}: HTTP ${r.status}`);
  const j = await r.json();
  if (j.error) throw new Error(`${method}: ${j.error.message}`);
  return j.result;
}

// Same as daemon() but against an explicit node URL ('' = same-origin proxy).
export const daemonAt = (node, method, params) => call(`${node || ''}/json_rpc`, method, params);
export const daemon = (method, params) => daemonAt(getNode(), method, params);
