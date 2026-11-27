// The time limit, from the Rust rules. `StarCycle::max_frames` (pow/sm64pow/src/rules.rs) is compiled into the perturbation
// WASM (pow/perturb-wasm includes that very file), so the page computes the node's number and not a port of it: from a tip's
// timestamp and retarget difficulty and the clock, the limit a block found now must meet. That is why the page no longer asks
// the node for a fresh template every few seconds (the limit grows with time since the parent).
//
// Everything here needs `await limitReady()` first (main.jsx does it before the first render).
import { rules_max_frames, rules_due_elapsed, rules_max_run_frames } from '../wasm/perturb/sm64coin_perturb_wasm.js';
import { perturbReady } from './perturbInit.js';

export const limitReady = (src) => perturbReady(src);

const U64_MAX = (1n << 64n) - 1n;
const bigDifficulty = (d) => {
  const v = typeof d === 'bigint' ? d : BigInt(Math.max(1, Math.floor(Number(d) || 1)));
  return v < 1n ? 1n : v > U64_MAX ? U64_MAX : v;
};
const bigSeconds = (s) => BigInt(Math.max(0, Math.floor(Number.isFinite(s) ? s : 0)));

/** The limit in frames for a block timestamped `elapsed` seconds after its parent at retarget difficulty `d` (number, BigInt or numeric string). */
export function maxFrames(d, elapsed) {
  return rules_max_frames(bigDifficulty(typeof d === 'string' ? BigInt(d) : d), bigSeconds(elapsed));
}

/** The most frames any block can carry. */
export const maxRunFrames = () => rules_max_run_frames();

/** The first second after the parent a run of `frames` fits the limit at retarget difficulty `d` (null: never). */
export function dueElapsed(d, frames) {
  const r = rules_due_elapsed(bigDifficulty(typeof d === 'string' ? BigInt(d) : d), frames >>> 0);
  return r === U64_MAX ? null : Number(r);
}

/** v0.4 proof archive window: a run is archivable as a proof for a height if it would have become valid at most ARCHIVE_SECONDS of
 *  auction time after the winner's due time (records::window_frames). */
export const ARCHIVE_SECONDS = 60;
/** The longest run within `seconds` of auction time after a winner of `winnerFrames` at retarget difficulty `d`. */
export function windowFrames(d, winnerFrames, seconds = ARCHIVE_SECONDS) {
  const due = dueElapsed(d, winnerFrames);
  return due === null ? winnerFrames : Math.max(maxFrames(d, due + seconds), winnerFrames);
}

/** The limit a block found at unix second `nowSec` must meet on top of `tip` ({timestamp, difficulty}); 0 without a tip. */
export function limitNow(tip, nowSec) {
  return tip ? maxFrames(tip.difficulty, nowSec - tip.timestamp) : 0;
}

/** The first unix second a run of `frames` is valid on top of `tip` (what the template's sm64_valid_at says; 0: never, the node
 *  decides). `launch`: the chain's launch time (mainnet), before which no block is valid. `restartDelay`: a restart (a power-on
 *  start on a parent that did not end its cycle) is judged against the limit that many seconds earlier (sm64pow chain.rs; the
 *  template's sm64_restart_delay), so it is valid that much later; the node's sm64_valid_at is the parent-start answer. */
export function validAtFor(tip, frames, launch = 0, restartDelay = 0) {
  if (!tip) return 0;
  const due = dueElapsed(tip.difficulty, frames);
  if (due === null) return 0;
  return Math.max(tip.timestamp + (restartDelay || 0) + due, tip.timestamp + 1, launch || 0);
}

/** A run's restart delay on template `t`: the template's sm64_restart_delay when the run starts from power-on although the
 *  template allows the parent's start (a reset cycle), else 0. `startKind`: 'power-on' or 'parent'. */
export function restartDelayFor(t, startKind) {
  return startKind === 'power-on' && t && t.sm64_start !== 'power-on' ? Number(t.sm64_restart_delay) || 0 : 0;
}
