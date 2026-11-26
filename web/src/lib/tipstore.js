// The last few cycle tips (the game at the end of a block, lib/cycle.js reachTip) kept in
// IndexedDB, so reloading the Mine page doesn't replay the cycle from power-on. A tip is
// stored as the memory pages that differ from power-on; a change of game build (another
// power-on) discards them.
const DB = 'sm64coin', STORE = 'tips', KEEP = 4;

function open() {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === 'undefined') { reject(new Error('no IndexedDB')); return; }
    const r = indexedDB.open(DB, 1);
    r.onupgradeneeded = () => r.result.createObjectStore(STORE, { keyPath: 'hash' });
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

/** A cheap fingerprint of the power-on memory: which game build the tips belong to. */
export function buildId(powerOn) {
  let h = powerOn.mem.top >>> 0;
  const pages = powerOn.mem.pages;
  for (let i = 0; i < pages.length; i += 61) {
    const w = new Uint32Array(pages[i].buffer, pages[i].byteOffset, pages[i].byteLength >> 2);
    for (let k = 0; k < w.length; k += 97) h = (Math.imul(h, 31) + w[k]) >>> 0;
  }
  return h;
}

/** Saves a tip ({hash, frames, state}); keeps the newest KEEP. Errors are ignored (it's a cache). */
export async function saveTip(engine, tip) {
  try {
    const base = engine.powerOn.mem.pages;
    const diffs = [];
    tip.state.mem.pages.forEach((p, i) => { if (i >= base.length || p !== base[i]) diffs.push([i, p]); });
    const db = await open();
    await tx(db, 'readwrite', (s) => s.put({ hash: tip.hash, frames: tip.frames, top: tip.state.mem.top, build: buildId(engine.powerOn), at: Date.now(), diffs }));
    const all = await tx(db, 'readonly', (s) => s.getAll());
    all.sort((a, b) => b.at - a.at);
    if (all.length > KEEP) await tx(db, 'readwrite', (s) => { all.slice(KEEP).forEach((t) => s.delete(t.hash)); });
    db.close();
  } catch { /* no storage: the cycle is just replayed */ }
}

/** The saved tips of this game build, oldest first, as {hash, frames, state}. */
export async function loadTips(engine) {
  try {
    const db = await open();
    const all = await tx(db, 'readonly', (s) => s.getAll());
    db.close();
    const id = buildId(engine.powerOn);
    const base = engine.powerOn.mem.pages;
    return all.filter((t) => t.build === id).sort((a, b) => a.at - b.at).map((t) => {
      const pages = base.slice();
      for (const [i, p] of t.diffs) pages[i] = p;
      const zero = new Uint8Array(4096);
      for (let i = 0; i < pages.length; i++) if (!pages[i]) pages[i] = zero;
      return { hash: t.hash, frames: t.frames, state: { mem: { pages, top: t.top }, pert: null } };
    });
  } catch {
    return [];
  }
}
