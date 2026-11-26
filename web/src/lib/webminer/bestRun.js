// Run sharing's held run (controller.js): the one run a template submits is the shortest any worker found; it is held
// PRIVATELY until `lead` seconds before the node's time limit has grown to it, while the workers look for a shorter one.
// One run per key (v0.3): the template's miner key signs and submits that one run, once, and never another for the
// same seed (a second different signed run is equivocation: honest nodes avoid the block). Pure logic, no browser
// APIs (bestRun.test.mjs drives it with a fake clock).

import { windowFrames } from '../rules.js';

/** Default seconds before a run is valid at which it is submitted (stamped with the second it becomes valid). */
export const DEFAULT_LEAD = 5;
/** The most a run may be sent ahead: restricted RPC refuses a run due more than 30 s after it arrives (rules.rs RPC_SUBMIT_AHEAD_SECONDS); ahead gains nothing. */
export const MAX_LEAD = 25;
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
    /** Forget a seed whose submission the node answered with a refusal (it stored nothing, so a shorter run may still be signed). */
    forget(seed) {
      if (!(seed in map)) return;
      delete map[seed];
      try { storage?.setItem(key, JSON.stringify(map)); } catch { /* private window: in memory */ }
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

/** The longest a template waits for the height's proofs (v0.4 sm64_records_ready_at); a node whose clock is far off must not stall the miner. */
export const MAX_RECORDS_WAIT = 120;

/**
 * v0.4: build the template for a new tip only after the node's `sm64_records_ready_at` (the tip's eligible time + window
 * + grace), so the block includes the previous height's proofs. `t` is a get_block_template answer; while its
 * ready time is ahead, sleeps until then (at most `max` s in all, in slices of 1 s so `current()` can end it) and asks
 * `fetch()` again. Returns the answer to use, or null if `current()` turned false.
 *   fetch()  async: the template again (same key and payout address)
 *   now() (s), sleep(ms), current(), onWait(seconds) (once)
 */
export async function waitRecordsReady(t, d) {
  const max = d.max ?? MAX_RECORDS_WAIT;
  const deadline = d.now() + max;
  let said = false;
  for (;;) {
    if (!d.current()) return null;
    const ready = Number(t.sm64_records_ready_at) || 0, n = d.now();
    const until = Math.min(ready, deadline);
    if (ready <= n || n >= deadline) return t;
    if (!said) { said = true; d.onWait?.(Math.ceil(until - n)); }
    await d.sleep(Math.min(until - n, 1) * 1000);
    if (d.now() >= until) t = await d.fetch();
  }
}

/**
 * v0.4 stage B: a run that lost its height (another block became the tip first, it was displaced, or it was refused) is
 * still a proof for that height: signed with the template's key and offered with submit_sm64_proof. ONE RUN PER KEY still
 * applies (a different run already signed for the seed is never signed), and it must be within the archive window
 * (windowFrames; the node has the last word). `d`:
 *   template, run   the template ({height, sm64_seed_hash, sm64_retarget_difficulty, blocktemplate_blob, minerKey}) and the run ({proof hex, frames})
 *   signed          signedSeeds() store
 *   winnerFrames(height)  async: frames of the chain's block at that height (null: unknown, the node decides)
 *   sign(proofBytes) async -> sig; seal(proofBytes, sig) -> submit hex; submit(hex) async -> {result, reason}
 * Returns {kind: 'sent', result, reason} | 'outside' (outside the window) | 'second' (another run signed) | 'error'.
 */
export async function offerAsProof(d) {
  const { template: t, run: r } = d;
  const other = d.signed.signedOther(t.sm64_seed_hash, r.proof);
  if (other !== null) return { kind: 'second', frames: other };
  let w = null;
  try { w = await d.winnerFrames(t.height); } catch { /* unknown: the node decides */ }
  if (w) {
    const limit = windowFrames(Number(t.sm64_retarget_difficulty || t.difficulty), w);
    if (r.frames > limit) return { kind: 'outside', winner: w, limit };
  }
  d.signed.record(t.sm64_seed_hash, r.proof, r.frames);
  try {
    const bytes = d.proofBytes(r.proof);
    const res = await d.submit(d.seal(bytes, await d.sign(bytes)));
    return { kind: 'sent', result: Number(res?.result ?? -1), reason: res?.reason || '' };
  } catch (e) {
    return { kind: 'error', error: String(e) };
  }
}

/** The node's verdict on a proof (submit_sm64_proof's result). */
export const proofVerdict = (v) => ({ 1: 'in the pool', 0: "ignored (known, not for the node's tip height, outside the window, or pool full)", '-2': 'the node lacks the parent state', '-3': 'bad encoding' }[v] || 'refused');
