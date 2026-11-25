// Virtual (on-screen) controller state, merged into the pad the game reads every frame.
//
// The Recorder asks the engine for the frame's pad (keyboard / gamepad), calls applyTouch(pad), then
// runs it through the perturbation; the pad that was actually played is what gets recorded and
// proved. So a run played by touch is an ordinary run: same 4-byte pad per frame, no special path.
// N64 button masks (sm64-binds-frontend's GamepadButtons; literal here so this file has no imports).
export const BUTTONS = {
  A: 0x8000, B: 0x4000, Z: 0x2000, R: 0x0010, L: 0x0020, START: 0x1000,
  CU: 0x0008, CD: 0x0004, CL: 0x0002, CR: 0x0001,
};

// Full stick deflection in OSContPad units (what the sm64 port's keyboard gives; walking to
// running is the magnitude, 64+ runs).
export const STICK_MAX = 80;

export const touchState = { buttons: 0, x: 0, y: 0 };

export function resetTouch() { touchState.buttons = 0; touchState.x = 0; touchState.y = 0; }

/** ORs the touch buttons into `pad` (a GamePad) and lets a held virtual stick override its stick. */
export function applyTouch(pad) {
  const t = touchState;
  if (t.buttons) pad.button |= t.buttons;
  if (t.x || t.y) { pad.stick_x = t.x; pad.stick_y = t.y; }
  return pad;
}

/** A thumb offset (dx right, dy DOWN, px) from the stick origin -> stick values (x right, y up),
 * with a dead zone and full deflection at `radius`. Magnitude is preserved (analog walk/run). */
export function stickFromOffset(dx, dy, radius, dead = 0.12) {
  const d = Math.hypot(dx, dy);
  if (d < 1e-6) return [0, 0];
  const m = Math.min(1, d / radius);
  if (m < dead) return [0, 0];
  const s = ((m - dead) / (1 - dead)) * STICK_MAX; // rescaled so it ramps from 0 past the dead zone
  return [Math.round((dx / d) * s) || 0, Math.round((-dy / d) * s) || 0];
}
