// The parent block's end state for the workers (get_sm64_state: ~6.7 MB hex-gzip, 8.5 MB inflated), fetched once per
// hash: a template refresh or a restart on the same parent reuses it (the node rate-limits this call: a byte budget
// shared with get_sm64_cycle, "Busy" over it), a "Busy" answer is retried after a pause before the caller falls back to
// rebuilding the parent from the cycle. On a cross-origin-isolated page the state lives in a SharedArrayBuffer, so
// every worker reads the same memory (no copy per worker); otherwise each worker gets its own copy, transferred.
import { daemon } from '../rpc.js';
import { hexToBytes } from '../proof.js';
import { mark } from '../perf.js';

// A save state is ~8 MiB; never inflate more than this (a gzip bomb from a hostile node).
const MAX_STATE_BYTES = 32 << 20;
const KEEP = 2;
const RETRY_MS = [1500, 4000];
const cache = new Map(); // hash -> Promise<ArrayBuffer | SharedArrayBuffer>
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Reads a stream to bytes, failing as soon as it passes `max` bytes.
async function readCapped(stream, max) {
  const parts = [];
  const r = stream.getReader();
  let n = 0;
  for (;;) {
    const { done, value } = await r.read();
    if (done) break;
    if (n + value.length > max) { r.cancel().catch(() => {}); throw new Error(`state larger than ${max} bytes`); }
    parts.push(value);
    n += value.length;
  }
  const iso = typeof SharedArrayBuffer !== 'undefined' && globalThis.crossOriginIsolated === true;
  const out = new Uint8Array(iso ? new SharedArrayBuffer(n) : new ArrayBuffer(n));
  let o = 0;
  for (const p of parts) { out.set(p, o); o += p.length; }
  return out.buffer;
}

async function fetchState(hash) {
  for (let k = 0; ; k++) {
    try {
      mark('state-fetch-begin');
      const s = await daemon('get_sm64_state', { hash });
      mark('state-fetched');
      const gz = new Blob([hexToBytes(s.state_gz)]).stream().pipeThrough(new DecompressionStream('gzip'));
      const buf = await readCapped(gz, MAX_STATE_BYTES);
      mark('state-decompressed');
      return buf;
    } catch (e) {
      if (k >= RETRY_MS.length || !/busy/i.test(String(e))) throw e;
      await sleep(RETRY_MS[k]);
    }
  }
}

/** The parent's end state (promise of an ArrayBuffer, a SharedArrayBuffer on an isolated page), fetched at most once per hash. */
export function parentState(hash) {
  let p = cache.get(hash);
  if (!p) {
    p = fetchState(hash);
    cache.set(hash, p);
    p.catch(() => { if (cache.get(hash) === p) cache.delete(hash); }); // a failure is tried again next time
    while (cache.size > KEEP) cache.delete(cache.keys().next().value);
  }
  return p;
}

/** What to post to a worker, and its transfer list: the shared buffer as is, else a private copy that is transferred. */
export function forWorker(buf) {
  if (typeof SharedArrayBuffer !== 'undefined' && buf instanceof SharedArrayBuffer) return [buf, []];
  const c = buf.slice(0);
  return [c, [c]];
}
