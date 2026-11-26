// A block's start state in the visual engine: the parent's end state as the node saved it (the headless
// SaveState bytes the page already fetched for the miners) turned into a portable state by the headless
// game in lib/nodestate.worker.js, then loaded into the visual engine. Power-on is the engine's frame 0.
import { restoreState, captureState } from '../../lib/savestate.js';

let worker = null;
let nextId = 1;
const jobs = new Map();
function getWorker() {
  if (worker) return worker;
  const w = (worker = new Worker(new URL('../../lib/nodestate.worker.js', import.meta.url), { type: 'module' }));
  w.onmessage = (e) => {
    const j = jobs.get(e.data.id);
    jobs.delete(e.data.id);
    if (j) (e.data.error ? j.reject(new Error(e.data.error)) : j.resolve(e.data.portable));
  };
  w.onerror = (e) => {
    for (const j of jobs.values()) j.reject(new Error(e.message || 'the state worker failed'));
    jobs.clear();
    worker = null;
  };
  return w;
}

export function portableFromRaw(raw) {
  const copy = raw instanceof ArrayBuffer ? raw.slice(0) : new Uint8Array(raw).slice().buffer; // a copy (the state may be a SharedArrayBuffer): it is transferred
  const id = nextId++;
  return new Promise((resolve, reject) => {
    jobs.set(id, { resolve, reject });
    getWorker().postMessage({ id, state: copy }, [copy]);
  });
}

// Converted states kept per round (hub.portable): the block's start and the newest few step start states. Every
// step the reel or a click plays converts its own, so without a bound a long round kept one per step played.
const KEEP = 4;

/** The portable state of `raw` for the visual game (a promise), converted once per `key` while it stays cached. */
export function portableFor(hub, key, raw) {
  if (!(hub.portable instanceof Map)) hub.portable = new Map();
  const m = hub.portable;
  let p = m.get(key);
  if (p) { m.delete(key); m.set(key, p); return p; } // most recently used last
  p = portableFromRaw(raw);
  p.catch(() => { if (m.get(key) === p) m.delete(key); }); // a failed conversion is tried again next time, not cached
  m.set(key, p);
  while (m.size > KEEP) m.delete(m.keys().next().value);
  return p;
}

export function stopStateWorker() {
  if (worker) worker.terminate();
  worker = null;
  jobs.clear();
}

/** Puts `engine` at the start of the hub's block: returns a saved state of it, or throws. */
export async function loadStart(engine, hub, kind) {
  if (kind === 'power-on' || !hub.parentRaw) {
    if (kind !== 'power-on') throw new Error('the parent\'s end state is not available');
    restoreState(engine, null, engine.powerOn);
    return captureState(engine, null, engine.powerOn);
  }
  const portable = await portableFor(hub, `block:${hub.round}`, hub.parentRaw);
  restoreState(engine, null, engine.powerOn);
  engine.load_portable_state(portable);
  return captureState(engine, null, engine.powerOn);
}
