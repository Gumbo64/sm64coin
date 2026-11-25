// Run sharing's held run (controller.js): the one run a template submits is the shortest any worker found; it is held
// PRIVATELY until `lead` seconds before the node's time limit has grown to it, while the workers look for a shorter one.
// One run per key (v0.3): the template's miner key signs and submits that one run, once, and never another for the
// same seed (a second different signed run is equivocation: honest nodes avoid the block). Pure logic, no browser
// APIs (bestRun.test.mjs drives it with a fake clock).

/** Default seconds before a run is valid at which it is submitted (stamped with the second it becomes valid). */
export const DEFAULT_LEAD = 5;
/** The most a run may be sent ahead (the nodes' future window is 3600 s, but ahead gains nothing): the lead is clamped to this. */
export const MAX_LEAD = 60;
export const clampLead = (n) => { const x = Number(n); return Number.isFinite(x) ? Math.min(MAX_LEAD, Math.max(0, Math.floor(x))) : DEFAULT_LEAD; };
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
 *   tipHash         the chain's tip when the template was made (usually its parent, not when the node builds around
 *                   an equivocated tip): another tip makes the run worthless
 *   lead            seconds before the run is valid to return it (default DEFAULT_LEAD)
 *   current()       false once the miner stopped or moved to another template
 *   sleep(ms), now() (s), clock() (ms)
 *   onHeld(run, validAt, changed)  each wait while held; `changed`: first wait for this run
 * Returns {kind: 'valid', run, validAt} (submit it now), {kind: 'stale', run} (a new tip) or {kind: 'gone'}.
 * The validity is asked again whenever the best run changed (a shorter run is valid sooner).
 */
export async function holdUntilValid(d) {
  let validAt = 0, asked = -1, said = -1;
  let lastTip = d.clock();
  const lead = clampLead(d.lead ?? DEFAULT_LEAD);
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
    if (validAt - d.now() <= lead) return { kind: 'valid', run: r, validAt };
    d.onHeld(r, validAt, said !== r.frames);
    said = r.frames;
    await d.sleep(Math.min(5, Math.max(0.25, validAt - d.now() - lead)) * 1000);
    if (!d.current()) return { kind: 'gone' };
    if (d.clock() - lastTip >= TIP_EVERY) {
      lastTip = d.clock();
      let tip = null;
      try { tip = await d.tip(); } catch { /* node hiccup: look again next time */ }
      if (!d.current()) return { kind: 'gone' };
      if (tip && tip !== d.tipHash) return { kind: 'stale', run: d.best() };
    }
  }
}

/** A cheap fingerprint of a run's proof (hex), to tell "the same run" from another one. */
export function runPrint(proofHex) {
  let h1 = 0xdeadbeef, h2 = 0x41c6ce57;
  for (let i = 0; i < proofHex.length; i++) {
    const c = proofHex.charCodeAt(i);
    h1 = Math.imul(h1 ^ c, 2654435761);
    h2 = Math.imul(h2 ^ c, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return (4294967296 * (2097151 & h2) + (h1 >>> 0)).toString(16);
}

/**
 * One run per key: which seeds a run was signed for (seed hash -> {print, frames}). `storage` is a localStorage-like
 * {getItem, setItem} (may be null or throw: then it lives in memory only, still guarding this page). Signing the very
 * same run again is allowed (the signature is deterministic: the same block).
 */
export function signedSeeds(storage, key = 'sm64coin.signedSeeds', keep = 64) {
  let map = {};
  try { map = JSON.parse(storage?.getItem(key) || '{}') || {}; } catch { map = {}; }
  return {
    /** null if `proofHex` may be signed for `seed`, else the frames of the different run already signed for it. */
    signedOther(seed, proofHex) {
      const old = map[seed];
      return old && old.print !== runPrint(proofHex) ? old.frames : null;
    },
    /** Remember it BEFORE signing. */
    record(seed, proofHex, frames) {
      map[seed] = { print: runPrint(proofHex), frames, at: Date.now() };
      const keys = Object.keys(map);
      if (keys.length > keep) for (const k of keys.sort((a, b) => map[a].at - map[b].at).slice(0, keys.length - keep)) delete map[k];
      try { storage?.setItem(key, JSON.stringify(map)); } catch { /* private window: in memory */ }
    },
  };
}
