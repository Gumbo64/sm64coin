// The reel's consistency check: a replayed position against the one the search recorded (no imports: unit-testable).
/** How far (game units) a replayed position may be from the recorded one: both are the same float32 game state, so this is rounding only. */
export const POS_TOL = 0.5;

/** {ok, dist} of position `got` against the recorded `want` ([x, y, z]); a missing `want` is ok (nothing to check against). */
export function checkPos(got, want, tol = POS_TOL) {
  if (!want || !got) return { ok: true, dist: 0, checked: false };
  const dist = Math.hypot(got[0] - want[0], got[1] - want[1], got[2] - want[2]);
  return { ok: dist <= tol, dist, checked: true };
}

