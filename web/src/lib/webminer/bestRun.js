// Run sharing's held run (controller.js): the one run a template submits is the shortest any worker found; it is held
// until the node's time limit has grown to it while the workers look for a shorter one. Pure logic, no browser APIs
// (bestRun.test.mjs drives it with a fake clock).

/** The nodes' future-timestamp window (120 s), with a margin: a run valid within this many seconds is sent now. */
export const SEND_AHEAD = 100;
/** How often (ms) a held run looks at the node's tip itself (the controller's watch also restarts rounds). */
export const TIP_EVERY = 30000;

/** A worker's found run against the best so far: {best, kind: 'first' | 'shorter' | 'ignored'} (only a strictly shorter run counts). */
export function offerRun(best, run) {
  if (best && !(run.frames < best.frames)) return { best, kind: 'ignored' };
  return { best: run, kind: best ? 'shorter' : 'first' };
}

/**
 * Holds the best run until it is valid. `d`:
 *   best()          the current best run ({frames, ...}; a shorter one may replace it at any await)
 *   validAt(frames) async: the node's time (s) a run of that length is valid at (0 / throw: the node decides, i.e. now)
 *   tip()           async: the node's top block hash (throw / null: unknown, look again later)
 *   prevHash        the template's parent: another tip makes the run worthless
 *   current()       false once the miner stopped or moved to another template
 *   sleep(ms), now() (s), clock() (ms)
 *   onHeld(run, validAt, changed)  each wait while held; `changed`: first wait for this run
 * Returns {kind: 'valid', run, validAt} (submit it now), {kind: 'stale', run} (a new tip) or {kind: 'gone'}.
 * The validity is asked again whenever the best run changed (a shorter run is valid sooner).
 */
export async function holdUntilValid(d) {
  let validAt = 0, asked = -1, said = -1;
  let lastTip = d.clock();
  for (;;) {
    if (!d.current()) return { kind: 'gone' };
    const r = d.best();
    if (asked !== r.frames) {
      try {
        validAt = (await d.validAt(r.frames)) || 0;
      } catch { validAt = 0; /* the node decides */ }
      asked = r.frames;
      continue; // (the best run may have changed meanwhile)
    }
    if (validAt - d.now() <= SEND_AHEAD) return { kind: 'valid', run: r, validAt };
    d.onHeld(r, validAt, said !== r.frames);
    said = r.frames;
    await d.sleep(Math.min(5, Math.max(1, validAt - d.now() - SEND_AHEAD)) * 1000);
    if (!d.current()) return { kind: 'gone' };
    if (d.clock() - lastTip >= TIP_EVERY) {
      lastTip = d.clock();
      let tip = null;
      try { tip = await d.tip(); } catch { /* node hiccup: look again next time */ }
      if (!d.current()) return { kind: 'gone' };
      if (tip && tip !== d.prevHash) return { kind: 'stale', run: d.best() };
    }
  }
}
