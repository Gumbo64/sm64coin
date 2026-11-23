// The browser's copy of the success condition, used only to know when to stop
// recording. The daemon's rules (pow/sm64pow/src/rules.rs) are the authority:
// if you change Rules::is_success there, change it here and add the new rules
// name. The progress points themselves come from pow/perturb (WASM,
// lib/perturb.js readProgress): one per star, per Bowser key, and for the
// Grand Star with 70+ stars.
//
// star-cycle-70: a block is the next progress point from the start state.
export const KNOWN_RULES = {
  // `start` and `now` are readProgress() results.
  'star-cycle-70': (start, now) => now.points > start.points,
};

export function successFor(rulesName) {
  return KNOWN_RULES[rulesName] || null;
}

// The longest run any block can carry (the time limit at difficulty 1:
// base_frames in rules.rs). Past this a run can never become valid.
export const MAX_RUN_FRAMES = 20 * 60 * 30;

// A power-on restart (starting over when no miner can do the next star yet)
// is judged against the limit this many seconds earlier, so it is allowed
// that long after the parent: a base plus a price per progress point thrown
// away, capped (StarCycle::restart_delay in pow/sm64pow/src/rules.rs; display
// only, the template's sm64_power_on_at is authoritative).
export const RESTART_BASE_SECONDS = 3600;
export const RESTART_POINT_SECONDS = 60;
export const RESTART_MAX_SECONDS = 2 * 3600;
export function restartDelay(points) {
  return Math.min(RESTART_BASE_SECONDS + points * RESTART_POINT_SECONDS, Math.max(RESTART_MAX_SECONDS, RESTART_BASE_SECONDS));
}
