// The "reset cycle" (power-on start) policy; the same decisions as pow/miner/src/reset.rs.
// The chain says what is allowed (template sm64_start): "power-on" (forced), "parent" (only the
// parent's end state until sm64_power_on_at), "either" (both).
export const ALL_STARS = 120;
export const RESET_MODES = ['never', 'when-stuck', 'always-when-allowed'];

/** {start: 'power-on'|'parent', why} for the first attempt on a template. */
export function initialStart(mode, sm64Start, parentStars) {
  if (sm64Start === 'power-on') return { start: 'power-on', why: 'the chain requires the power-on start' };
  if (sm64Start === 'either') {
    if (mode === 'always-when-allowed') return { start: 'power-on', why: 'reset allowed and policy is always-when-allowed' };
    if (mode === 'when-stuck' && parentStars >= ALL_STARS) return { start: 'power-on', why: 'the parent already has every star' };
    return { start: 'parent', why: 'parent first' };
  }
  return { start: 'parent', why: 'the chain allows only the parent\'s start' };
}

/** After a parent-start search found no run: switch to power-on? */
export function afterFailure(mode, sm64Start) {
  return sm64Start === 'either' && mode !== 'never';
}

/** Unix time at which the miner should refresh the template (a "parent" one turns "either"), or 0. */
export function restartAt(mode, sm64Start, powerOnAt) {
  return sm64Start === 'parent' && powerOnAt > 0 && mode !== 'never' ? powerOnAt : 0;
}

/** Seconds until power-on is allowed (0 when it is, or forced). Only a "parent" template waits. */
export function secondsUntilAllowed(sm64Start, powerOnAt, now) {
  return sm64Start === 'parent' ? Math.max(0, Math.ceil(powerOnAt - now)) : 0;
}
