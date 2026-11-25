// One shared tree (rrt::mine with `shared_tree`): N workers run the same search, same salt, as copies of
// each other (the search is deterministic given its seed). Each round's attempts (the expensive part: a GA
// rollout of 10^4-10^5 frames each) are claimed one by one through this SharedArrayBuffer, each worker
// publishes the encoded results of those it ran (obrrt_search::wire) and, once all of the round's are in,
// reads them all in attempt order, so every copy merges the same results in the same order whichever worker
// ran which attempt. Needs a cross-origin-isolated page (Atomics.wait in workers).
//
// Layout (Int32 words): [0] claim counter (global attempt index, monotonic over rounds and steps),
// [1] readers done (monotonic, workers x rounds), [2] rounds begun, [3] stop flag, [4],[5] the two result
// areas' bump pointers; the result table from byte TABLE (ENTRIES x {tag, offset, length}); the two result
// areas (rounds alternate: a round starts only when every worker has read the one before the last).
const TABLE = 256;
const ENTRIES = 4096;
const DATA_AT = TABLE + ENTRIES * 12;
const WAIT_MS = 100;

export const attemptsSize = (dataBytes = 48 << 20) => DATA_AT + 2 * dataBytes;
export const stopAttempts = (sab) => { Atomics.store(new Int32Array(sab), 3, 1); Atomics.notify(new Int32Array(sab), 3); };

export function makeAttempts(sab, slot, slots) {
  const w = new Int32Array(sab);
  const bytes = new Uint8Array(sab);
  const dataBytes = (sab.byteLength - DATA_AT) / 2;
  let step = -1; // the step in progress (a new instance's rounds count from 1)
  let cur = null; // {round, base, n, parity}
  let totalBase = 0;
  let gRound = 0;

  const stopped = () => { if (Atomics.load(w, 3) !== 0) throw new Error('shared search stopped'); };
  // Blocks until the Int32 word `i` of w satisfies `ok(value)`.
  const until = (i, ok) => {
    for (;;) {
      const v = Atomics.load(w, i);
      if (ok(v)) return;
      stopped();
      Atomics.wait(w, i, v, WAIT_MS);
    }
  };

  const begin = (round, n) => {
    if (n > ENTRIES / 2) throw new Error(`round of ${n} attempts is too large for the shared table`);
    cur = { round, base: totalBase, n, parity: 0 };
    totalBase += n;
    gRound += 1;
    cur.parity = gRound & 1;
    until(1, (v) => v >= slots * (gRound - 1));
    if (slot === 0) {
      Atomics.store(w, 4 + cur.parity, 0);
      Atomics.store(w, 2, gRound);
      Atomics.notify(w, 2);
    } else {
      until(2, (v) => v >= gRound);
    }
  };

  return {
    /** A new campaign step (a fresh instance): its rounds count from 1 again. */
    newStep() { step += 1; cur = null; },
    claim(round, n) {
      if (!cur || cur.round !== round) begin(round, n);
      for (;;) {
        const c = Atomics.load(w, 0);
        if (c >= cur.base + cur.n) return -1;
        if (Atomics.compareExchange(w, 0, c, c + 1) === c) return c - cur.base;
      }
    },
    put(round, i, data) {
      const len = data.length;
      const at = Atomics.add(w, 4 + cur.parity, (len + 7) & ~7);
      if (at + len > dataBytes) throw new Error(`the round's results (${(at + len) >> 20} MB) do not fit the shared area`);
      bytes.set(data, DATA_AT + cur.parity * dataBytes + at);
      const g = cur.base + i;
      const e = (g % ENTRIES) * 3 + TABLE / 4;
      Atomics.store(w, e + 1, at);
      Atomics.store(w, e + 2, len);
      Atomics.store(w, e, g + 1);
      Atomics.notify(w, e);
    },
    /** Waits for all of the round's results and keeps copies for `get`; then this worker counts as done reading. */
    wait(round, n) {
      if (!cur || cur.round !== round) begin(round, n);
      const out = new Array(n);
      for (let i = 0; i < n; i++) {
        const g = cur.base + i;
        const e = (g % ENTRIES) * 3 + TABLE / 4;
        until(e, (v) => v === g + 1);
        const at = Atomics.load(w, e + 1);
        const len = Atomics.load(w, e + 2);
        out[i] = bytes.slice(DATA_AT + cur.parity * dataBytes + at, DATA_AT + cur.parity * dataBytes + at + len);
      }
      cur.results = out;
      Atomics.add(w, 1, 1);
      Atomics.notify(w, 1);
    },
    get(round, i) { return cur.results[i]; },
  };
}
