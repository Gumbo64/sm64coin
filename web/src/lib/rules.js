// The browser's copy of the success condition, used only to know when to stop
// recording. The daemon's rules (pow/sm64pow/src/rules.rs) are the authority:
// if you change Rules::is_success there, change it here and add the new rules
// name. The progress points themselves come from pow/perturb (WASM,
// lib/perturb.js readProgress): one per star, per Bowser key, per cap switch
// pressed (v0.5) and for the Grand Star with 70+ stars.
//
// star-cycle-70-caps (v0.5): a block is the next progress point from the start state; the points are stars, Bowser
// keys, the three cap switches and the Grand Star (lib/perturb.js readProgress). (star-cycle-70, v0.2-v0.4: no caps.)
export const KNOWN_RULES = {
  // `start` and `now` are readProgress() results.
  'star-cycle-70-caps': (start, now) => now.points > start.points,
};

export function successFor(rulesName) {
  return KNOWN_RULES[rulesName] || null;
}

const CAP_SWITCHES = [[1, 'the wing cap switch (red)'], [2, 'the metal cap switch (green)'], [4, 'the vanish cap switch (blue)']];

/**
 * What a run added between two readProgress() results, for messages ("goal reached: the wing cap switch (red)"), so a
 * player can see a cap switch counted. The precedence of Rust's perturb sm64.rs `event`: Grand Star, star, key, cap.
 * null: no progress point.
 */
export function progressLabel(start, now) {
  if (!start || !now || !(now.points > start.points)) return null;
  if (now.grand && !start.grand) return 'the Grand Star';
  if (now.stars > start.stars) return 'a star';
  if (now.keys > start.keys) return `Bowser key ${now.keys}`;
  const caps = (now.caps || 0) & ~(start.caps || 0);
  const cap = CAP_SWITCHES.find(([bit]) => caps & bit);
  return cap ? cap[1] : 'a progress point';
}

// The longest run any block can carry (the time limit at difficulty 1:
// base_frames in rules.rs). Past this a run can never become valid.
export const MAX_RUN_FRAMES = 20 * 60 * 30;

// Seconds between blocks the retarget aims for (StarCycle::target in rules.rs; display only,
// e.g. a wait-time estimate: the daemon's get_info block_target is the authority).
export const BLOCK_TARGET_SECONDS = 1200;

// A power-on restart (starting over when no miner can do the next star yet)
// is judged against the limit this many seconds earlier, so it is allowed
// that long after the parent: a base plus a price per progress point thrown
// away, capped (StarCycle::restart_delay in pow/sm64pow/src/rules.rs; display
// only, the template's sm64_power_on_at is authoritative).
export const RESTART_BASE_SECONDS = 4 * 3600; // v0.3: 12 block times (20-min blocks)
export const RESTART_POINT_SECONDS = 240;
export const RESTART_MAX_SECONDS = 8 * 3600;
export function restartDelay(points) {
  return Math.min(RESTART_BASE_SECONDS + points * RESTART_POINT_SECONDS, Math.max(RESTART_MAX_SECONDS, RESTART_BASE_SECONDS));
}

// The time limit itself (StarCycle::max_frames) is not here: lib/limit.js calls the Rust function through the perturbation WASM.
// What stays is constants for display (the node's get_info / template numbers are authoritative):
// MIN_RUN_FRAMES: the shortest limit (min_frames in rules.rs).
export const MIN_RUN_FRAMES = 20 * 30;
