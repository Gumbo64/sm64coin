import assert from 'node:assert/strict';
import { checkPos, POS_TOL } from './replayCheck.js';

assert.deepEqual(checkPos([1, 2, 3], [1, 2, 3]), { ok: true, dist: 0, checked: true });
assert.equal(checkPos([1, 2, 3], [1, 2, 3.4]).ok, true); // within rounding
assert.equal(checkPos([1, 2, 3], [1, 2, 3 + POS_TOL + 0.1]).ok, false);
const far = checkPos([-996, 397, 3168], [-2530, 581, 2056]); // the reel played without the perturbation: the bug's 1900 units
assert.equal(far.ok, false);
assert.ok(far.dist > 1900);
assert.equal(checkPos([1, 2, 3], null).checked, false); // nothing recorded: nothing to compare
console.log('replayCheck ok');
