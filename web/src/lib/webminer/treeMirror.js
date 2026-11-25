// The page-side sparse mirror of a worker's search tree (rrt::child::viz `Edges` is the sender): the nodes the cloud shows
// plus their ancestors, each with its parent id and the edge's pads. A node's path from the step's root is rebuilt by
// walking the parents, so a click (or a highlight clip) needs no round trip to the worker.
//
// An edge (little endian): u32 id, u32 parent (0xFFFFFFFF none), u16 runs, then per run u8 count, u16 button, i8 stickX,
// i8 stickY (run-length coded pads). Parents precede children. Node 0 is the step's root and has no edge.
//
// One mirror per worker and step: a new step key drops the old step's edges (they are of no use without its start state).
// Memory: the received chunks as they came (about 100 bytes a node) plus one index entry per edge.
// For hybrid's "Play from here": hub.nodePath(w, key, id) answers the raw pads of the node's path (4 bytes a frame) or null.

const NONE = 0xffffffff;

export class TreeMirror {
  constructor() {
    this.key = null;
    this.index = new Map(); // id -> [parent, chunk, offset of the first run, runs, frames]
    this.chunks = [];
    this.bytes = 0;
  }

  /** Adds the edges in `buf` (ArrayBuffer/Uint8Array) of step `key`; a different key than before starts a new mirror. Returns the edges added. */
  add(key, buf) {
    if (key !== this.key) { this.key = key; this.index = new Map(); this.chunks = []; this.bytes = 0; }
    const b = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
    const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
    const ci = this.chunks.length;
    let at = 0, n = 0;
    while (at + 10 <= b.length) {
      const id = dv.getUint32(at, true);
      const parent = dv.getUint32(at + 4, true);
      const runs = dv.getUint16(at + 8, true);
      const off = at + 10;
      let frames = 0;
      for (let r = 0; r < runs; r++) frames += b[off + r * 5];
      this.index.set(id, [parent === NONE ? -1 : parent, ci, off, runs, frames]);
      at = off + runs * 5;
      n++;
    }
    this.chunks.push(b);
    this.bytes += b.length;
    return n;
  }

  has(key, id) { return key === this.key && (id === 0 || this.index.has(id)); }

  /** Frames of the path to `id`, or -1 when an ancestor is missing. */
  frames(key, id) {
    if (key !== this.key) return -1;
    let total = 0;
    for (let at = id, guard = 0; at !== 0; guard++) {
      const e = this.index.get(at);
      if (!e || guard > 100000) return -1;
      total += e[4];
      at = e[0];
      if (at < 0) break;
    }
    return total;
  }

  /** The pads of the path from the root to `id`: raw bytes, 4 per frame (u16 LE buttons, i8 x, i8 y); null when an ancestor is missing. */
  path(key, id) {
    const total = this.frames(key, id);
    if (total < 0) return null;
    const out = new Uint8Array(total * 4);
    let end = total * 4;
    for (let at = id; at > 0;) {
      const e = this.index.get(at);
      const [parent, ci, off, runs, frames] = e;
      const c = this.chunks[ci];
      let o = end - frames * 4;
      for (let r = 0; r < runs; r++) {
        const p = off + r * 5;
        const n = c[p], b0 = c[p + 1], b1 = c[p + 2], sx = c[p + 3], sy = c[p + 4];
        for (let k = 0; k < n; k++, o += 4) { out[o] = b0; out[o + 1] = b1; out[o + 2] = sx; out[o + 3] = sy; }
      }
      end -= frames * 4;
      at = parent;
    }
    return out;
  }

  get size() { return this.index.size; }
}
