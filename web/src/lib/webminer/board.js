// A bulletin board the workers of a page share through a SharedArrayBuffer (needs a
// cross-origin-isolated page): one slot per worker, each holding that worker's latest message
// (JSON). Writers and readers never block: a seqlock per slot (odd sequence number = being
// written; a reader retries until it sees the same even number before and after its copy).
const HEADER = 8; // Int32 seq, Int32 length
const enc = new TextEncoder();
const dec = new TextDecoder();

// After the slots: Int32 control words shared by the page and every worker: [0] = the viz feed is on.
const CTL = 64;
// After the control words: the page's mailbox to the workers (requests for a search node's inputs), one JSON text
// the page rewrites as a whole under the same seqlock: [Int32 seq, Int32 length, bytes].
const MAIL = 256 << 10;
export const boardSize = (slots, slotBytes = 1 << 20) => slots * slotBytes + CTL + MAIL;

/** The page's switch for the workers' viz feed (instant: the searches read it every round). */
export function setVizFlag(sab, slots, on, slotBytes = 1 << 20) {
  Atomics.store(new Int32Array(sab, slots * slotBytes, 1), 0, on ? 1 : 0);
}

/** The page's mailbox write: `obj` as JSON for every worker's next poll; false when it doesn't fit. */
export function postMail(sab, slots, obj, slotBytes = 1 << 20) {
  const base = slots * slotBytes + CTL;
  const ints = new Int32Array(sab, base, 2);
  const b = enc.encode(JSON.stringify(obj));
  if (b.length > MAIL - HEADER) return false;
  const s = Atomics.load(ints, 0);
  Atomics.store(ints, 0, (s + 1) | 1);
  new Uint8Array(sab, base + HEADER, b.length).set(b);
  Atomics.store(ints, 1, b.length);
  Atomics.store(ints, 0, (s + 2) & ~1);
  return true;
}

/** The board over `sab` (boardSize(slots) bytes) for the worker at `slot`. */
export function makeBoard(sab, slot, slots, slotBytes = 1 << 20) {
  const ints = new Int32Array(sab);
  const bytes = new Uint8Array(sab);
  const at = (i) => (i * slotBytes);
  const ctl = new Int32Array(sab, slots * slotBytes, 1);
  const read = (i) => {
    const base = at(i);
    for (let tries = 0; tries < 1000; tries++) {
      const s1 = Atomics.load(ints, base / 4);
      if (s1 === 0) return null; // nothing posted
      if (s1 & 1) continue;
      const len = Atomics.load(ints, base / 4 + 1);
      const copy = bytes.slice(base + HEADER, base + HEADER + len);
      if (Atomics.load(ints, base / 4) === s1) return dec.decode(copy);
    }
    return null;
  };
  return {
    slot,
    slots: Array.from({ length: slots }, (_, i) => i),
    read,
    /** The page's mailbox (see postMail) as parsed JSON, or null. */
    mail() {
      const base = slots * slotBytes + CTL;
      const mi = new Int32Array(sab, base, 2);
      for (let tries = 0; tries < 1000; tries++) {
        const s1 = Atomics.load(mi, 0);
        if (s1 === 0) return null;
        if (s1 & 1) continue;
        const len = Atomics.load(mi, 1);
        const copy = bytes.slice(base + HEADER, base + HEADER + len);
        if (Atomics.load(mi, 0) === s1) { try { return JSON.parse(dec.decode(copy)); } catch { return null; } }
      }
      return null;
    },
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

/** Every worker's current message as [slot, text] (own included). */
function entries(board) {
  return board.slots.map((i) => [i, board.read(i)]).filter(([, t]) => t != null);
}

/**
 * Typed messages over the board: each worker's slot holds one JSON object {cp, tree} (the latest
 * message of each kind), so the checkpoint sync and the tree sync don't overwrite each other.
 */
export function makeChannel(board) {
  const mine = {};
  const peers = (kind, withSelf = false) => {
    const out = [];
    for (const [slot, t] of entries(board)) {
      if (!withSelf && slot === board.slot) continue;
      try { const m = JSON.parse(t); if (m && m[kind] != null) out.push(m[kind]); } catch { /* torn or foreign */ }
    }
    return out;
  };
  return {
    post(kind, obj) { mine[kind] = obj; return board.put(JSON.stringify(mine)); },
    peers,
  };
}

/** The checkpoint sync (rrt::mine's "cp_sync"): publish `payload` ({tag, eta, cp}) and return the
 * cp (JSON) with the lowest ETA among the workers' messages with the same tag (the lowest slot on a tie). */
export function bestCheckpoint(chan, payload) {
  const mine = JSON.parse(payload);
  chan.post('cp', mine);
  let best = null;
  for (const m of chan.peers('cp', true)) {
    if (m.tag === mine.tag && (!best || m.eta < best.eta)) best = m;
  }
  return best ? JSON.stringify(best.cp) : undefined;
}

const RING_BYTES = 640 * 1024;

/**
 * The tree sync ("tree_sync", see rrt::child::tsync): `payload` is {tag, recs} (this worker's newest
 * frontier records for the step `tag`). The worker's recent records stay on the board (so a peer that
 * syncs later still sees them); the reply is {recs} of the other workers' records with the same tag that
 * this worker has not been given before, in slot order.
 */
export function makeTreeSync(chan) {
  let tag = null;
  let ring = [];
  const given = new Set();
  return (payload) => {
    const m = JSON.parse(payload);
    if (m.tag !== tag) { tag = m.tag; ring = []; given.clear(); }
    ring.push(...m.recs);
    let bytes = ring.reduce((n, r) => n + r.pads.length + 80, 0);
    while (bytes > RING_BYTES && ring.length > 1) bytes -= ring.shift().pads.length + 80;
    chan.post('tree', { tag, recs: ring });
    const recs = [];
    for (const t of chan.peers('tree')) {
      if (t.tag !== tag) continue;
      for (const r of t.recs) {
        if (given.has(r.pads)) continue;
        given.add(r.pads);
        recs.push(r);
      }
    }
    return JSON.stringify({ recs });
  };
}

/**
 * The page's node-path requests for this worker's search (`viz_req` in rrt::child::viz): the mailbox holds
 * {reqs: [{id, slot, key, nid}]}; the reply is the JSON list of node ids asked of this slot for the step `key`
 * that this worker has not been given yet.
 */
export function makeNodeRequests(board) {
  const given = new Set();
  return (key) => {
    const m = board.mail();
    if (!m || !m.reqs) return undefined;
    const ids = [];
    for (const r of m.reqs) {
      if (given.has(r.id) || r.slot !== board.slot || r.key !== key) continue;
      given.add(r.id);
      ids.push(r.nid);
    }
    return ids.length ? JSON.stringify(ids) : undefined;
  };
}
