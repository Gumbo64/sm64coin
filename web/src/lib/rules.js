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
