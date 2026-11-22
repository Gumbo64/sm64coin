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
export const MAX_RUN_FRAMES = 10 * 60 * 30;

// After this many seconds without a block, a block may start over from power-on
// (StarCycle::stall in pow/sm64pow/src/rules.rs; display only).
export const STALL_SECONDS = 3600;
