// The game's end state of each block (every star / key boundary of a cycle), kept so a replay of
// block N starts from block N-1's end state instead of fast-forwarding from power-on.
//  - memory: a small LRU of full states (lib/savestate.js format), newest last;
//  - IndexedDB: the memory pages that differ from power-on (as lib/tipstore.js does for tips),
//    keyed by block hash, and tagged with the game build (a different build's states are ignored).
// All failures are swallowed: this is a cache, the replay just fast-forwards without it.
import { buildId } from './tipstore.js';

const DB = 'sm64coin-blockstates', STORE = 'states', MEM_KEEP = 4, DISK_KEEP = 40;
const mem = new Map(); // `${build}:${hash}` -> state

function open() {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === 'undefined') { reject(new Error('no IndexedDB')); return; }
    const r = indexedDB.open(DB, 1);
    r.onupgradeneeded = () => r.result.createObjectStore(STORE, { keyPath: 'key' });
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}
const tx = (db, mode, f) => new Promise((resolve, reject) => {
  const t = db.transaction(STORE, mode);
  const out = f(t.objectStore(STORE));
  t.oncomplete = () => resolve(out && 'result' in out ? out.result : undefined);
  t.onerror = () => reject(t.error);
});

/** The cached end state of block `hash` ({mem, pert: null}) and where it came from ('memory' | 'disk'), or null. */
export async function getBlockState(powerOn, hash) {
  const key = `${buildId(powerOn)}:${hash}`;
  if (mem.has(key)) { const s = mem.get(key); mem.delete(key); mem.set(key, s); return { state: s, from: 'memory' }; }
  try {
    const db = await open();
    const t = await tx(db, 'readonly', (s) => s.get(key));
    db.close();
    if (!t) return null;
    const pages = powerOn.mem.pages.slice();
    for (const [i, p] of t.diffs) pages[i] = p;
    const zero = new Uint8Array(4096);
    for (let i = 0; i < pages.length; i++) if (!pages[i]) pages[i] = zero;
    const state = { mem: { pages, top: t.top }, pert: null };
    remember(key, state);
    return { state, from: 'disk' };
  } catch { return null; }
}

function remember(key, state) {
  mem.delete(key);
  mem.set(key, state);
  while (mem.size > MEM_KEEP) mem.delete(mem.keys().next().value);
}

/** Keeps block `hash`'s end state (memory now, disk in the background). */
export async function putBlockState(powerOn, hash, state) {
  const build = buildId(powerOn);
  const key = `${build}:${hash}`;
  remember(key, state);
  try {
    const base = powerOn.mem.pages;
    const diffs = [];
    state.mem.pages.forEach((p, i) => { if (i >= base.length || p !== base[i]) diffs.push([i, p]); });
    const db = await open();
    await tx(db, 'readwrite', (s) => s.put({ key, hash, build, top: state.mem.top, at: Date.now(), diffs }));
    const all = await tx(db, 'readonly', (s) => s.getAll());
    if (all.length > DISK_KEEP) {
      all.sort((a, b) => b.at - a.at);
      await tx(db, 'readwrite', (s) => { all.slice(DISK_KEEP).forEach((t) => s.delete(t.key)); });
    }
    db.close();
  } catch { /* no storage */ }
}

export const forgetBlockStates = () => mem.clear(); // tests
