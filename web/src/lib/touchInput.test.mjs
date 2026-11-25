import assert from 'node:assert/strict';
import { applyTouch, stickFromOffset, touchState, resetTouch, BUTTONS, STICK_MAX } from './touchInput.js';

const eq = (a, b) => assert.deepEqual(a, b);
// dead zone, centre, full deflection, magnitude ordering (walk < run), directions (y up)
eq(stickFromOffset(0, 0, 64), [0, 0]);
eq(stickFromOffset(5, 0, 64), [0, 0]);
eq(stickFromOffset(64, 0, 64), [STICK_MAX, 0]);
eq(stickFromOffset(500, 0, 64), [STICK_MAX, 0]);
eq(stickFromOffset(0, -64, 64), [0, STICK_MAX]); // thumb up = stick up
eq(stickFromOffset(0, 64, 64), [0, -STICK_MAX]);
const walk = stickFromOffset(26, 0, 64)[0], run = stickFromOffset(50, 0, 64)[0];
assert.ok(walk > 0 && walk < 40 && run > 55 && run <= STICK_MAX, `${walk} ${run}`);
const diag = stickFromOffset(45, -45, 64);
assert.ok(Math.hypot(...diag) <= STICK_MAX + 1);

// applyTouch ORs buttons and overrides the stick only while touched
const pad = { button: 0x0010, stick_x: 3, stick_y: -4 };
resetTouch(); applyTouch(pad); eq(pad, { button: 0x0010, stick_x: 3, stick_y: -4 });
touchState.buttons = BUTTONS.A | BUTTONS.Z; touchState.x = 70; touchState.y = -10; applyTouch(pad);
eq(pad, { button: 0x0010 | 0x8000 | 0x2000, stick_x: 70, stick_y: -10 });
console.log('touchInput ok');
