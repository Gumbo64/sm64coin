// The cycle that ends at a block, computed in this browser instead of by a node: the same
// answer as the daemon's get_sm64_cycle ({hash, start_height, blocks: [{height, hash, frames,
// power_on, event}], played_pads (hex), stars, keys, grand_star, ends_cycle}), from the blocks'
// proofs (plain get_block, cheap for a node) replayed in a Web Worker (worker.js, core.js).
// Blocks never change, so each block's replay inputs and played pads are kept in IndexedDB by
// hash: a visitor replays a block once, ever. Needs the ROM (the game is built from it); without
// it, or if anything fails, the node answers (getCycle falls back to get_sm64_cycle).
import { daemon } from '../rpc.js';
import { blockProof, splitProof } from './core.js';

const DB = 'sm64coin-replay';
let dbp = null;
function db() {
  dbp ??= new Promise((resolve, reject) => {
    const r = indexedDB.open(DB, 1);
    r.onupgradeneeded = () => {
      r.result.createObjectStore('blocks');
      r.result.createObjectStore('played');
    };
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
  return dbp;
}
async function idbGet(store, key) {
  const d = await db();
  return new Promise((resolve, reject) => {
    const r = d.transaction(store).objectStore(store).get(key);
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}
async function idbPut(store, key, value) {
  const d = await db();
  return new Promise((resolve, reject) => {
    const t = d.transaction(store, 'readwrite');
    t.objectStore(store).put(value, key);
    t.oncomplete = () => resolve();
    t.onerror = () => reject(t.error);
  });
}

const blockMem = new Map(); // hash -> Promise of blockInfo (in flight or done: each block is fetched once)
/** What replaying a block needs (and its place in the chain), fetched once per block. */
function blockInfo(hash) {
  if (!blockMem.has(hash)) {
    const p = fetchBlockInfo(hash);
    blockMem.set(hash, p);
    p.catch(() => blockMem.delete(hash));
  }
  return blockMem.get(hash);
}
async function fetchBlockInfo(hash) {
  let b = await idbGet('blocks', hash).catch(() => null);
  if (!b) {
    const blk = await daemon('get_block', { hash });
    const proof = blockProof(blk);
    const { frames, start, codec } = splitProof(proof);
    b = {
      hash, height: blk.block_header.height, prev: blk.block_header.prev_hash, frames, start, codec, proof,
      blk: {
        sm64_seed_hash: blk.sm64_seed_hash, sm64_window_length: blk.sm64_window_length, sm64_random_amount: blk.sm64_random_amount,
        sm64_burst_length: blk.sm64_burst_length, sm64_a_prob: blk.sm64_a_prob, sm64_b_prob: blk.sm64_b_prob, sm64_z_prob: blk.sm64_z_prob,
        sm64_timing_mix: blk.sm64_timing_mix, sm64_pad_mix: blk.sm64_pad_mix, sm64_pads: blk.sm64_pads || '',
      },
    };
    idbPut('blocks', hash, b).catch(() => {});
  }
  return b;
}

// Every replayed block's result, whoever asked (two components asking for the same cycle at
// once: the worker replays it for the first; the second finds the results here).
const playedMem = new Map();

let worker = null;
let nextId = 1;
const jobs = new Map();
function getWorker() {
  if (worker) return worker;
  worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
  worker.onmessage = (e) => jobs.get(e.data.id)?.(e.data);
  worker.onerror = (e) => { for (const f of jobs.values()) f({ type: 'error', message: e.message || 'replay worker failed' }); jobs.clear(); worker = null; };
  return worker;
}

const hex = (bytes) => {
  let s = '';
  for (let i = 0; i < bytes.length; i++) s += (bytes[i] < 16 ? '0' : '') + bytes[i].toString(16);
  return s;
};

const BATCH = 64;
const PARALLEL = 8;

async function mapLimit(xs, n, f) {
  const out = new Array(xs.length);
  let i = 0;
  await Promise.all(Array.from({ length: Math.min(n, xs.length) }, async () => {
    while (i < xs.length) { const k = i++; out[k] = await f(xs[k]); }
  }));
  return out;
}

/**
 * The blocks of the cycle that ends at `tip`, oldest first (as get_sm64_cycle walks them).
 * Each block names its parent, so walking back one get_block at a time is one round trip per
 * block; instead the main chain's hashes come from get_block_headers_range (one call per
 * BATCH blocks) and the blocks are fetched PARALLEL at a time. A tip off the main chain (the
 * headers don't link up) is walked one block at a time.
 */
async function cycleBlocks(tip) {
  const out = [];
  let b = await blockInfo(tip);
  for (;;) {
    if (b.height === 0) break; // the genesis block isn't played
    out.push(b);
    if (b.start === 1 || b.height === 1) break;
    if (!blockMem.has(b.prev)) {
      const end = b.height - 1;
      const start = Math.max(1, end - BATCH + 1);
      try {
        const r = await daemon('get_block_headers_range', { start_height: start, end_height: end });
        const hs = r.headers || [];
        // Main chain and linked to this block? Then prefetch them all (newest first: the walk stops early at a power-on start).
        if (hs.length && hs[hs.length - 1].hash === b.prev) await mapLimit(hs.map((h) => h.hash).reverse(), PARALLEL, blockInfo);
      } catch { /* the walk below fetches one at a time */ }
    }
    b = await blockInfo(b.prev);
  }
  return out.reverse();
}

/**
 * The cycle ending at `tip`, replayed here. `onProgress(done, total)` while replaying (frames).
 * Throws if the replay disagrees with the chain (a block that doesn't add exactly one point,
 * a non-canonical proof): the caller falls back to the node.
 */
export async function clientCycle(tip, onProgress = () => {}) {
  getWorker().postMessage({ id: 0, items: [] }); // starts the game while the blocks download
  const blocks = await cycleBlocks(tip);
  const played = await Promise.all(blocks.map((b) => playedMem.get(b.hash) || idbGet('played', b.hash).catch(() => null)));
  if (played.some((p) => !p)) {
    const items = blocks.map((b, i) => (played[i]
      ? { hash: b.hash, height: b.height, start: b.start, frames: b.frames, pads: played[i].pads.slice(0) }
      : { hash: b.hash, height: b.height, start: b.start, frames: b.frames, blk: b.blk, proof: splitProof(b.proof) }));
    const id = nextId++;
    await new Promise((resolve, reject) => {
      jobs.set(id, (m) => {
        if (m.type === 'progress') onProgress(m.done, m.total);
        else if (m.type === 'block') {
          const i = blocks.findIndex((b) => b.hash === m.hash);
          if (!m.canonical || m.gained !== 1) { jobs.delete(id); reject(new Error(`block ${blocks[i]?.height}: the replay disagrees with the chain`)); return; }
          played[i] = { pads: m.pads, poked: m.poked, frames: m.frames, event: m.event, progress: m.progress };
          playedMem.set(m.hash, played[i]);
          idbPut('played', m.hash, played[i]).catch(() => {});
        } else if (m.type === 'done') {
          jobs.delete(id);
          // Blocks the worker had already played for another request.
          blocks.forEach((b, i) => { played[i] ??= playedMem.get(b.hash); });
          if (played.some((p) => !p)) reject(new Error('the replay left blocks out'));
          else resolve();
        }
        else if (m.type === 'error') { jobs.delete(id); reject(new Error(m.message)); }
      });
      getWorker().postMessage({ id, items }, items.filter((it) => it.pads).map((it) => it.pads));
    });
  }
  const total = played.reduce((s, p) => s + p.frames, 0);
  const all = new Uint8Array(total * 4);
  let at = 0;
  for (const p of played) { all.set(new Uint8Array(p.pads), at); at += p.pads.byteLength; }
  const last = played[played.length - 1]?.progress || [0, 0, 0, 0];
  return {
    hash: tip,
    start_height: blocks.length ? blocks[0].height : 0,
    blocks: blocks.map((b, i) => ({ height: b.height, hash: b.hash, frames: played[i].frames, power_on: b.start === 1, event: played[i].event })),
    played_pads: hex(all),
    stars: last[0], keys: last[1], grand_star: last[2] === 1, ends_cycle: last[2] === 1,
    poked: played.map((p) => new Uint8Array(p.poked)),
    client: true,
  };
}

// Whether the visitor stored the ROM (sm64-binds-frontend keeps it in IndexedDB
// SM64_LIB_WASM_DB / files, key 'ROM'): a key lookup, not isRomCached (which reads all 8 MB).
// Remembered once true; "no ROM" is asked again, since the visitor may upload one.
let romKnown = false;
function romStored() {
  return new Promise((resolve) => {
    const r = indexedDB.open('SM64_LIB_WASM_DB', 1);
    r.onupgradeneeded = () => r.result.createObjectStore('files', { keyPath: 'id' }); // as the library does
    r.onerror = () => resolve(false);
    r.onsuccess = () => {
      try {
        const q = r.result.transaction('files').objectStore('files').getKey('ROM');
        q.onsuccess = () => resolve(q.result !== undefined);
        q.onerror = () => resolve(false);
      } catch { resolve(false); }
    };
  });
}
const hasRom = async () => romKnown || (romKnown = await romStored());

/**
 * The cycle ending at `hash` with its played pads: replayed in this browser when it has the ROM
 * (nodes do no replay work for it), else (or on any failure) the node's get_sm64_cycle.
 */
export async function getCycle(hash, onProgress) {
  if (typeof Worker !== 'undefined' && (await hasRom())) {
    try {
      return await clientCycle(hash, onProgress);
    } catch (e) {
      console.warn('client-side replay failed, asking the node:', e);
    }
  }
  return daemon('get_sm64_cycle', { hash });
}
