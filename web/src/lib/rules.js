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

// The time limit (StarCycle::max_frames in pow/sm64pow/src/rules.rs, the same
// integer arithmetic; display only, the template's sm64_max_frames is
// authoritative): at retarget difficulty `d`, `elapsed` seconds after the
// parent's timestamp. One difficulty halving (8 levels of 2%) per 60 s.
export const MIN_RUN_FRAMES = 20 * 30;
// START_HALVINGS: the limit starts that many halvings below the retarget's (StarCycle::start_halvings).
const SHRINK_PERCENT = 98n, DECAY_HALVING = 60n, DECAY_GRACE = 0n, START_HALVINGS = 20n;
export function level8(d) {
  let v = BigInt(d);
  if (v <= 1n) return 0n;
  let msb = 0n;
  while ((v >> (msb + 1n)) > 0n) msb += 1n;
  const frac = msb >= 3n ? (v >> (msb - 3n)) & 7n : (v << (3n - msb)) & 7n;
  return msb * 8n + frac;
}
export function maxFrames(d, elapsed) {
  const t = DECAY_HALVING;
  const e = BigInt(Math.max(0, Math.floor(elapsed)));
  const decay = (e > DECAY_GRACE ? e - DECAY_GRACE : 0n) * 8n;
  let level = (level8(d) + 8n * START_HALVINGS) * t - decay;
  if (level < 0n) level = 0n;
  const whole = level / t, frac = level % t;
  let frames = BigInt(MAX_RUN_FRAMES);
  const min = BigInt(MIN_RUN_FRAMES);
  for (let i = 0n; i < whole; i++) {
    frames = (frames * SHRINK_PERCENT) / 100n;
    if (frames <= min) return MIN_RUN_FRAMES;
  }
  frames = (frames * (100n * t - (100n - SHRINK_PERCENT) * frac)) / (100n * t);
  return Number(frames > min ? frames : min);
}

// v0.4 proof archive window (docs/V0.4-DESIGN.md decision 3): a run is archivable as a proof for a height if it would
// have become valid at most ARCHIVE_SECONDS of auction time after the winner's due time (records::window_frames).
export const ARCHIVE_SECONDS = 60;
/** The first second after the parent a run of `frames` fits the limit at retarget difficulty `d` (null: never). */
export function dueElapsed(d, frames) {
  if (maxFrames(d, 2 ** 40) < frames) return null;
  let lo = 0, hi = 2 ** 40;
  while (lo < hi) {
    const mid = Math.floor((lo + hi) / 2);
    if (maxFrames(d, mid) >= frames) hi = mid; else lo = mid + 1;
  }
  return lo;
}
/** The longest run within `seconds` of auction time after a winner of `winnerFrames` at retarget difficulty `d`. */
export function windowFrames(d, winnerFrames, seconds = ARCHIVE_SECONDS) {
  const due = dueElapsed(d, winnerFrames);
  return due === null ? winnerFrames : Math.max(maxFrames(d, due + seconds), winnerFrames);
}
