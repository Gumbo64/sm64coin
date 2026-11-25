import assert from 'node:assert/strict';
import { SlotBank, makeSlot, slotKey, SLOT_COUNT, courseLabel } from './saveslots.js';

const pads = Array.from({ length: 10 }, (_, i) => ({ button: i, stick_x: 0, stick_y: 0 }));
const poked = pads.map((_, i) => i % 3 === 0);
const snaps = [{ frame: 0 }, { frame: 4 }, { frame: 8 }];

// the slot keeps a COPY of the pad prefix up to its frame
const s = makeSlot({ frame: 6, pads, poked, snaps, state: { x: 1 }, info: { stars: 2 } });
assert.equal(s.pads.length, 6); assert.equal(s.poked.length, 6);
assert.deepEqual(s.pads.map((p) => p.button), [0, 1, 2, 3, 4, 5]);
assert.deepEqual(s.snaps.map((x) => x.frame), [0, 4]); // saves after the slot are not part of it
pads.length = 3; // the recording being truncated later does not touch the slot
assert.equal(s.pads.length, 6);
assert.throws(() => makeSlot({ frame: 9, pads, poked, snaps, state: {} }));

// bank: put/get/clear, and template binding
const b = new SlotBank('a|power-on');
assert.equal(b.count, SLOT_COUNT);
assert.equal(b.get(0), null);
b.put(0, s); b.put(3, s);
assert.equal(b.get(0), s);
assert.equal(b.rebind('a|power-on'), false); assert.equal(b.get(0), s); // same template: kept
assert.equal(b.rebind('b|power-on'), true);                              // another template: emptied
assert.ok(b.slots.every((x) => x === null));

assert.notEqual(slotKey({ sm64_seed_hash: 'h1' }, { kind: 'parent' }), slotKey({ sm64_seed_hash: 'h1' }, { kind: 'power-on' }));
assert.equal(courseLabel(1), 'Bob-omb Battlefield'); assert.equal(courseLabel(99), 'Course 99');
console.log('saveslots ok');
