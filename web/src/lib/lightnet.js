// The website's light-client network (docs/LIGHT-CLIENTS.md): in a plain browser tab the default (public) node path goes through
// pow/lc-wasm, which keeps a scored list of nodes (HTTPS nodes now; iroh nodes over a relay when public-node.json lists them),
// fails over between them, fans submissions out to several nodes, bans a node that lies, and verifies the chain from a
// checkpoint compiled into the release (headers, ids, timestamps and the consensus rules; no Mario replay in the page).
// The WASM is loaded lazily after the first paint; until it is ready, and when it fails to load or no node answers, lib/rpc.js
// uses the single public node exactly as before. A node the user chose by hand never goes through here.
//
// Everything is injected (the wasm loader, storage, timers) so the logic runs under node (lightnet.test.mjs).

const LIST_KEY = 'sm64coin.lcnodes';
export const REFRESH_MS = 30_000;
export const VERIFY_MS = 10 * 60_000;
const FIRST_VERIFY_MS = 4_000;
/** Methods whose result a single node's answer is enough for, sent to several nodes at once. */
export const SUBMIT_METHODS = new Set(['submit_block', 'submit_sm64_proof']);

/** The bootstrap entries of public-node.json (`{"nodes": [...], "url": "..."}`), https/http URLs and `iroh:<id>@<relay>`. */
export function bootstrapEntries(json) {
  const j = json && typeof json === 'object' ? json : {};
  const all = [...(Array.isArray(j.nodes) ? j.nodes : []), ...(j.url ? [j.url] : [])];
  return all.map((s) => String(s || '').trim()).filter((s) => /^(https?:\/\/[^\s@?#]+|iroh:[0-9a-fA-F]{64}(@\S+)?)$/.test(s));
}
/** The first https/http entry: what the rest of the site calls "the public node". */
export const firstHttpNode = (json) => (bootstrapEntries(json).find((s) => /^https?:/.test(s)) || '').replace(/\/+$/, '');
export const hasIrohNodes = (json) => bootstrapEntries(json).some((s) => s.startsWith('iroh:'));

/** Thrown when the light-client path cannot serve a call: the caller uses its own single-node path instead. */
export class Unavailable extends Error {
  constructor(why) { super(why); this.name = 'LightnetUnavailable'; this.unavailable = true; }
}

const defaultLoad = async () => {
  const m = await import('../wasm/lc/sm64coin_lc_wasm.js');
  await m.default();
  return m;
};

/**
 * deps: { loadWasm(): Promise<module with LightClient>, genesis, storage (localStorage-like or null), setTimeout, clearTimeout,
 *         setInterval, clearInterval, now }
 */
export function createLightnet(deps = {}) {
  const loadWasm = deps.loadWasm || defaultLoad;
  const store = deps.storage === undefined ? (typeof localStorage !== 'undefined' ? localStorage : null) : deps.storage;
  const later = deps.setTimeout || ((f, ms) => setTimeout(f, ms));
  const every = deps.setInterval || ((f, ms) => setInterval(f, ms));
  const nowMs = deps.now || (() => Date.now());
  const genesis = deps.genesis || '';

  let state = 'idle'; // idle | loading | ready | failed
  let failure = '';
  let mod = null;
  let lc = null;
  let started = null;
  let verifyInfo = null;
  let verifying = false;
  const listeners = new Set();
  const emit = () => listeners.forEach((f) => { try { f(); } catch { /* a listener must not break the client */ } });

  const read = () => { try { return (store && store.getItem(LIST_KEY)) || ''; } catch { return ''; } };
  const write = (s) => { try { if (store) store.setItem(LIST_KEY, s); } catch { /* storage unavailable */ } };
  const persist = () => { if (lc) write(lc.list_json()); };

  async function refresh() {
    if (!lc) return;
    try { await lc.refresh(); } catch { /* the next tick */ }
    persist();
    emit();
  }
  async function verify() {
    if (!lc || verifying) return verifyInfo;
    verifying = true;
    try {
      verifyInfo = JSON.parse(await lc.verify());
    } catch (e) {
      verifyInfo = { level: 'failed', error: String((e && e.message) || e), at: Math.floor(nowMs() / 1000) };
    } finally {
      verifying = false;
    }
    persist();
    emit();
    return verifyInfo;
  }

  /** Starts loading (idempotent). `bootstrap`: the parsed public-node.json. Resolves when ready or failed; never rejects. */
  function start(bootstrap) {
    if (started) return started;
    state = 'loading';
    started = (async () => {
      try {
        mod = await loadWasm();
        lc = await mod.LightClient.create(JSON.stringify(bootstrap || {}), genesis, read());
        state = 'ready';
        persist();
        emit();
        every(() => { if (typeof document === 'undefined' || !document.hidden) refresh(); }, REFRESH_MS);
        every(() => { if (typeof document === 'undefined' || !document.hidden) verify(); }, VERIFY_MS);
        refresh().then(() => later(verify, FIRST_VERIFY_MS));
      } catch (e) {
        state = 'failed';
        failure = String((e && e.message) || e);
        emit();
      }
    })();
    return started;
  }

  /** Can this call go through the network? (ready, and the method is on the node allowlist.) */
  const available = (method) => state === 'ready' && !!mod.LightClient.allowed(method);

  const nodeError = (method, j) => new Error(`${method}: ${j.error.message ?? 'error'}`);

  /** JSON-RPC `method(params)` -> result, failing over across nodes. Throws Unavailable when the caller should use its own path. */
  async function call(method, params = {}) {
    if (!available(method)) throw new Unavailable(state === 'ready' ? `${method} is not served by the light-client network` : `light-client network ${state}`);
    const p = JSON.stringify(params === undefined ? {} : params);
    if (SUBMIT_METHODS.has(method)) {
      let outs;
      try { outs = JSON.parse(await lc.submit(method, p)); } catch (e) { throw new Unavailable(String((e && e.message) || e)); }
      const answers = outs.filter((o) => o.ok).map((o) => { try { return JSON.parse(o.answer); } catch { return null; } }).filter(Boolean);
      const good = answers.find((j) => !j.error);
      if (good) return good.result;
      if (answers.length) throw nodeError(method, answers[0]);
      throw new Unavailable(outs.map((o) => o.error).filter(Boolean).join('; ') || 'no node took the submission');
    }
    let text;
    try { text = await lc.rpc(method, p); } catch (e) { throw new Unavailable(String((e && e.message) || e)); }
    let j;
    try { j = JSON.parse(text); } catch { throw new Unavailable(`${method}: unreadable answer`); }
    if (j.error) throw nodeError(method, j);
    return j.result;
  }

  /** What the Network page shows: state, nodes with scores, verification. */
  function status() {
    const base = { state, failure, verify: verifyInfo };
    if (!lc) return { ...base, nodes: [] };
    let st = {};
    try { st = JSON.parse(lc.status()); } catch { /* none */ }
    const now = st.now || Math.floor(nowMs() / 1000);
    const nodes = ((st.list && st.list.records) || []).map((r) => ({
      key: r.addr.Http ? `http:${r.addr.Http.url}` : `iroh:${r.addr.Iroh.endpoint_id}`,
      kind: r.addr.Http ? 'https' : 'iroh',
      url: r.addr.Http ? r.addr.Http.url : '',
      relay: r.addr.Iroh ? r.addr.Iroh.relay_url : null,
      source: r.source, okRate: r.ok_rate, latencyMs: r.ewma_latency_ms, height: r.height, strikes: r.strikes,
      banned: r.banned_until > now,
      verified: Array.isArray(st.allowed) ? st.allowed.includes(r.addr.Http ? `http:${r.addr.Http.url}` : `iroh:${r.addr.Iroh.endpoint_id}`) : null,
    }));
    return { ...base, nodes, iroh: !!st.iroh };
  }

  return {
    start, call, available, status, refresh, verify,
    state: () => state,
    onChange: (f) => { listeners.add(f); return () => listeners.delete(f); },
    banLiar: (key) => { if (lc) { lc.ban_liar(key); persist(); emit(); } },
  };
}

/** The page's instance (rpc.js gives it the genesis hash through `configure`). */
let shared = null;
export const lightnet = () => shared || (shared = createLightnet({}));
export const configureLightnet = (deps) => { shared = createLightnet(deps); return shared; };
