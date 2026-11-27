// v0.5: the three cap switches are progress points (web copy of Rules::is_success / Progress::points).
import assert from 'node:assert/strict';
import { successFor, progressLabel } from './rules.js';
import { eventName } from './replay/core.js';

const ok = successFor('star-cycle-70-caps');
const s = { stars: 12, keys: 1, grand: false, points: 13, caps: 0 };
assert.equal(ok(s, { ...s, points: 14, caps: 1 }), true, 'a cap switch alone is a block');
assert.equal(ok({ ...s, points: 14, caps: 1 }, { ...s, points: 14, caps: 1 }), false, 'not twice');
assert.equal(ok(s, { ...s, stars: 13, points: 15, caps: 1 }), true, 'a star and a cap in one run');

// replay worker's event names: [stars, keys, grand, points, caps]
assert.equal(eventName([0, 0, 0, 0, 0], [0, 0, 0, 1, 2]), 'metal');
assert.equal(eventName([0, 0, 0, 0, 0], [0, 0, 0, 1, 1]), 'wing');
assert.equal(eventName([0, 0, 0, 0, 0], [1, 0, 0, 2, 1]), null); // a star and a cap in one run: the star is the event
assert.equal(eventName([0, 0, 0, 0, 0], [0, 0, 0, 1, 4]), 'vanish');
assert.equal(eventName([0, 0, 0, 1, 1], [0, 0, 0, 2, 3]), 'metal');
assert.equal(eventName([0, 0, 0, 0, 0], [0, 1, 0, 2, 1]), 'key1', 'a key and a cap: the key');
assert.equal(eventName([0, 0, 0, 0], [0, 0, 0, 0]), null);

// What the manual miner's "goal reached" message names (readProgress objects).
assert.equal(progressLabel(s, { ...s, points: 14, caps: 1 }), 'the wing cap switch (red)');
assert.equal(progressLabel({ ...s, points: 14, caps: 1 }, { ...s, points: 15, caps: 3 }), 'the metal cap switch (green)');
assert.equal(progressLabel(s, { ...s, points: 14, caps: 4 }), 'the vanish cap switch (blue)');
assert.equal(progressLabel(s, { ...s, stars: 13, points: 15, caps: 1 }), 'a star', 'a star and a cap: the star, as in Rust');
assert.equal(progressLabel(s, { ...s, keys: 2, points: 14 }), 'Bowser key 2');
assert.equal(progressLabel(s, s), null, 'no progress: nothing to name');
// The manual check and the label agree: every state the rules call a block gets a name.
for (const caps of [1, 2, 4]) assert.ok(ok(s, { ...s, points: 14, caps }) && progressLabel(s, { ...s, points: 14, caps }));
console.log('caps ok');
