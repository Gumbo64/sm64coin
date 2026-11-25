// The hybrid mining mailbox (board.js postMail / makeHybridPoll) and the page's command bookkeeping (hybrid.js).
import assert from 'node:assert/strict';
import { boardSize, makeBoard, postMail, makeHybridPoll } from './board.js';
import { Hybrid, encPads } from './hybrid.js';

const slots = 3;
const sab = new SharedArrayBuffer(boardSize(slots));
const b0 = makeBoard(sab, 0, slots);
const b1 = makeBoard(sab, 1, slots);
assert.equal(b0.mail(), null, 'nothing posted yet');

assert.ok(postMail(sab, slots, { goal: { level: 9, area: 1, pos: [1, 2, 3] }, cmds: [] }));
assert.deepEqual(b0.mail().goal, { level: 9, area: 1, pos: [1, 2, 3] });

const h = new Hybrid();
h.attach(sab, slots);
h.setGoal({ level: 5, area: 2, pos: [10, 20, 30] });
const p0 = makeHybridPoll(b0), p1 = makeHybridPoll(b1);
const poll = { tag: 'T', vkey: 'v0' };
assert.deepEqual(JSON.parse(p0(JSON.stringify(poll))), { goal: { level: 5, area: 2, pos: [10, 20, 30] }, cmds: [] });

// A state request reaches only the slot and step it names, once.
const st = h.requestState(1, 7, 'v1', 1000);
st.catch(() => {});
assert.equal(JSON.parse(p0(JSON.stringify({ tag: 'T', vkey: 'v1' }))).cmds.length, 0, 'other slot');
assert.equal(JSON.parse(p1(JSON.stringify({ tag: 'T', vkey: 'other' }))).cmds.length, 0, 'other step');
const got = JSON.parse(p1(JSON.stringify({ tag: 'T', vkey: 'v1' }))).cmds;
assert.equal(got.length, 1);
assert.deepEqual([got[0].kind, got[0].nid], ['state', 7]);
assert.equal(JSON.parse(p1(JSON.stringify({ tag: 'T', vkey: 'v1' }))).cmds.length, 0, 'given once');
h.onMessage(1, { kind: 'state', id: got[0].id, ok: true, nid: 7 }, new Uint8Array([1, 2, 3]));
const r = await st;
assert.deepEqual([...r.state], [1, 2, 3]);

// An insert goes to every worker on its tag (and a later step with that tag still gets it).
const replies = [];
const ins = h.insert({ tag: 'T', path: encPads([{ button: 1, stick_x: -1, stick_y: 127 }]), seg: encPads([{ button: 0x8000, stick_x: 0, stick_y: 0 }]), weight: 8 }, (slot, m) => replies.push([slot, m.ok]), 1000);
const c0 = JSON.parse(p0(JSON.stringify({ tag: 'T', vkey: 'a' }))).cmds;
const c1 = JSON.parse(p1(JSON.stringify({ tag: 'T', vkey: 'b' }))).cmds;
assert.equal(c0.length + c1.length, 2);
assert.equal(c0[0].path, '0001ff7f');
assert.equal(c0[0].seg, '80000000');
assert.equal(JSON.parse(p0(JSON.stringify({ tag: 'U', vkey: 'a' }))).cmds.length, 0, 'another tag');
h.onMessage(0, { kind: 'inserted', id: c0[0].id, ok: false, why: 'already in the tree' });
h.onMessage(1, { kind: 'inserted', id: c1[0].id, ok: true, nid: 12 });
assert.equal((await ins).slot, 1);
assert.deepEqual(replies, [[0, false], [1, true]]);
h.detach();
console.log('hybrid mailbox ok');
