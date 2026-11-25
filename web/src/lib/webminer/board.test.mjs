import assert from 'node:assert/strict';
import { boardSize, setVizFlag, postMail, makeBoard, makeChannel, makeNodeRequests } from './board.js';

// Small slots so the layout (slots, control words, mailbox) is easy to check.
const SLOT = 4096, N = 3;
const sab = new SharedArrayBuffer(boardSize(N, SLOT));
const boards = [0, 1, 2].map((i) => makeBoard(sab, i, N, SLOT));

// Nothing posted yet.
assert.equal(boards[0].mail(), null);
assert.equal(boards[1].read(0), null);
assert.equal(boards[0].vizOn(), false);

// The viz flag, the slots and the mailbox don't overlap.
setVizFlag(sab, N, true, SLOT);
assert.ok(boards[2].vizOn());
const chans = boards.map(makeChannel);
chans[0].post('cp', { tag: 'a', eta: 3 });
chans[2].post('cp', { tag: 'a', eta: 1 });
assert.ok(postMail(sab, N, { reqs: [{ id: 1, slot: 1, key: 'k', nid: 7 }] }, SLOT));
assert.ok(boards[0].vizOn());
assert.deepEqual(chans[1].peers('cp').map((m) => m.eta), [3, 1]);
assert.deepEqual(boards[0].mail(), { reqs: [{ id: 1, slot: 1, key: 'k', nid: 7 }] });

// Off and on again between two looks: a worker sees "off" once (so its sampler re-sends what the page dropped), then on.
setVizFlag(sab, N, false, SLOT);
setVizFlag(sab, N, true, SLOT);
assert.equal(boards[1].vizOn(), false);
assert.equal(boards[1].vizOn(), true);
assert.equal(boards[2].vizOn(), false); // every worker on its own
assert.equal(boards[2].vizOn(), true);
setVizFlag(sab, N, false, SLOT);
assert.equal(boards[1].vizOn(), false);
assert.equal(boards[1].vizOn(), false);
setVizFlag(sab, N, true, SLOT);
assert.equal(boards[1].vizOn(), true); // already saw this off
assert.equal(boards[0].vizOn(), false);
assert.equal(boards[0].vizOn(), true);
assert.deepEqual(boards[0].mail(), { reqs: [{ id: 1, slot: 1, key: 'k', nid: 7 }] }); // the counter is not the mailbox

// A rewrite replaces the whole text (a shorter one too) and keeps the sequence even.
assert.ok(postMail(sab, N, { reqs: [] }, SLOT));
assert.deepEqual(boards[2].mail(), { reqs: [] });
assert.equal(new Int32Array(sab, N * SLOT + 64, 1)[0] % 2, 0);
// Too big for the mailbox: refused, the old text stays.
assert.equal(postMail(sab, N, { pad: 'x'.repeat(300 << 10) }, SLOT), false);
assert.deepEqual(boards[1].mail(), { reqs: [] });

// Node requests: each worker gets its own slot's ids for its step key, each request once.
const req1 = makeNodeRequests(boards[1]);
const req2 = makeNodeRequests(boards[2]);
postMail(sab, N, { reqs: [{ id: 1, slot: 1, key: 'k', nid: 7 }, { id: 2, slot: 2, key: 'k', nid: 8 }, { id: 3, slot: 1, key: 'old', nid: 9 }] }, SLOT);
assert.equal(req1('k'), '[7]');
assert.equal(req1('k'), undefined); // already given
assert.equal(req2('k'), '[8]');
postMail(sab, N, { reqs: [{ id: 1, slot: 1, key: 'k', nid: 7 }, { id: 4, slot: 1, key: 'k', nid: 7 }] }, SLOT);
assert.equal(req1('k'), '[7]'); // asked again (a new request id): answered again
console.log('board ok');
