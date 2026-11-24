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
  const copy = raw.slice(0);
  const id = nextId++;
  return new Promise((resolve, reject) => {
    jobs.set(id, { resolve, reject });
    getWorker().postMessage({ id, state: copy }, [copy]);
  });
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
  hub.portable ??= {};
  hub.portable[hub.round] ??= portableFromRaw(hub.parentRaw);
  const portable = await hub.portable[hub.round];
  restoreState(engine, null, engine.powerOn);
  engine.load_portable_state(portable);
  return captureState(engine, null, engine.powerOn);
}
