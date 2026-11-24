// A bulletin board the workers of a page share through a SharedArrayBuffer (needs a
// cross-origin-isolated page): one slot per worker, each holding that worker's latest message
// (JSON). Writers and readers never block: a seqlock per slot (odd sequence number = being
// written; a reader retries until it sees the same even number before and after its copy).
const HEADER = 8; // Int32 seq, Int32 length
const enc = new TextEncoder();
const dec = new TextDecoder();

// After the slots: Int32 control words shared by the page and every worker: [0] = the viz feed is on.
const CTL = 64;
export const boardSize = (slots, slotBytes = 1 << 20) => slots * slotBytes + CTL;

/** The page's switch for the workers' viz feed (instant: the searches read it every round). */
export function setVizFlag(sab, slots, on, slotBytes = 1 << 20) {
  Atomics.store(new Int32Array(sab, slots * slotBytes, 1), 0, on ? 1 : 0);
}

/** The board over `sab` (boardSize(slots) bytes) for the worker at `slot`. */
export function makeBoard(sab, slot, slots, slotBytes = 1 << 20) {
  const ints = new Int32Array(sab);
  const bytes = new Uint8Array(sab);
  const at = (i) => (i * slotBytes);
  const ctl = new Int32Array(sab, slots * slotBytes, 1);
  return {
    /** Whether the page wants the viz feed now. */
    vizOn: () => Atomics.load(ctl, 0) === 1,
    /** Publishes this worker's message (dropped when it doesn't fit). */
    put(text) {
      const b = enc.encode(text);
      if (b.length > slotBytes - HEADER) return false;
      const base = at(slot);
      const s = Atomics.load(ints, base / 4);
      Atomics.store(ints, base / 4, s + 1 | 1);
      bytes.set(b, base + HEADER);
      Atomics.store(ints, base / 4 + 1, b.length);
      Atomics.store(ints, base / 4, (s + 2) & ~1);
      return true;
    },
    /** Every worker's current message (JSON text), in slot order. */
    all() {
      const out = [];
      for (let i = 0; i < slots; i++) {
        const base = at(i);
        for (let tries = 0; tries < 1000; tries++) {
          const s1 = Atomics.load(ints, base / 4);
          if (s1 === 0) break; // nothing posted
          if (s1 & 1) continue;
          const len = Atomics.load(ints, base / 4 + 1);
          const copy = bytes.slice(base + HEADER, base + HEADER + len);
          if (Atomics.load(ints, base / 4) === s1) { out.push(dec.decode(copy)); break; }
        }
      }
      return out;
    },
  };
}

/** The checkpoint sync (rrt::mine's "cp_sync"): publish `payload` ({tag, eta, cp}) and return the
 * cp (JSON) with the lowest ETA among the workers' messages with the same tag (the lowest slot on a tie). */
export function bestCheckpoint(board, payload) {
  board.put(payload);
  let best = null;
  const tag = JSON.parse(payload).tag;
  for (const t of board.all()) {
    let m;
    try { m = JSON.parse(t); } catch { continue; }
    if (m.tag === tag && (!best || m.eta < best.eta)) best = m;
  }
  return best ? JSON.stringify(best.cp) : undefined;
}
