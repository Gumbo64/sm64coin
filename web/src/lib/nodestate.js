// The end state of a block as the node saved it (get_sm64_state), brought into the visual
// engine: instead of replaying the cycle's pads from power-on, the node's headless SaveState is
// written into a headless game in a worker (nodestate.worker.js), exported as a portable state
// and loaded into the visual engine (Sm64VisualEngine.load_portable_state). Any failure throws;
// callers fall back to replaying the cycle.
import { daemon } from './rpc.js';
import { hexToBytes } from './proof.js';
import { restoreState } from './savestate.js';

async function gunzip(bytes) {
  return new Response(new Blob([bytes]).stream().pipeThrough(new DecompressionStream('gzip'))).arrayBuffer();
}

let worker = null;
let nextId = 1;
const jobs = new Map();
function getWorker() {
  if (worker) return worker;
  const w = (worker = new Worker(new URL('./nodestate.worker.js', import.meta.url), { type: 'module' }));
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

/** The node's saved end state of block `hash` as a portable state (Uint8Array). */
export async function fetchPortableState(hash) {
  const r = await daemon('get_sm64_state', { hash });
  if (!r || typeof r.state_gz !== 'string') throw new Error('the node sent no state');
  const raw = await gunzip(hexToBytes(r.state_gz));
  const id = nextId++;
  return new Promise((resolve, reject) => {
    jobs.set(id, { resolve, reject });
    getWorker().postMessage({ id, state: raw }, [raw]);
  });
}

/** Puts `engine` at the node's end state of block `hash` (from power-on, then the portable state). */
export async function loadNodeState(engine, hash) {
  const portable = await fetchPortableState(hash);
  restoreState(engine, null, engine.powerOn);
  engine.load_portable_state(portable);
}
