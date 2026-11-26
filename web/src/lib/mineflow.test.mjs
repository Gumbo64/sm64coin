import assert from 'node:assert/strict';
import { decideAfterWin, effectiveValidAt, launchFor, isNodeRefusal, refusalReason, confirmBlock, LAUNCH_TIMESTAMP } from './mineflow.js';

// launch applies on mainnet only; validAt never precedes it
assert.equal(launchFor({ mainnet: true }), LAUNCH_TIMESTAMP);
assert.equal(launchFor({ mainnet: false }), 0);
assert.equal(launchFor({ mainnet: false }, 500), 500);
assert.equal(effectiveValidAt(0, LAUNCH_TIMESTAMP), LAUNCH_TIMESTAMP);
assert.equal(effectiveValidAt(LAUNCH_TIMESTAMP + 60, LAUNCH_TIMESTAMP), LAUNCH_TIMESTAMP + 60);
assert.equal(effectiveValidAt(70, 0), 70);

const now = 1000;
// valid now: sent; over the limit / before launch: held
assert.deepEqual(decideAfterWin({ now, validAt: 0, frames: 900 }), { action: 'send', replaces: false });
assert.deepEqual(decideAfterWin({ now, validAt: 1000, frames: 900 }), { action: 'send', replaces: false });
assert.deepEqual(decideAfterWin({ now, validAt: 1100, frames: 900 }), { action: 'hold', replaces: false });
// a shorter run replaces the pending one, a longer one is dropped; equal valid time: fewer frames wins
const pend = { validAt: 1100, frames: 900 };
assert.deepEqual(decideAfterWin({ now, validAt: 1050, frames: 800, pending: pend }), { action: 'hold', replaces: true });
assert.deepEqual(decideAfterWin({ now, validAt: 900, frames: 700, pending: pend }), { action: 'send', replaces: true });
assert.equal(decideAfterWin({ now, validAt: 1200, frames: 950, pending: pend }).action, 'keep-pending');
assert.equal(decideAfterWin({ now, validAt: 1100, frames: 900, pending: pend }).action, 'keep-pending');
assert.deepEqual(decideAfterWin({ now, validAt: 1100, frames: 850, pending: pend }), { action: 'hold', replaces: true }); // both clamped to the launch
// a spent template (run sent / accepted): never another submission
assert.equal(decideAfterWin({ now, validAt: 0, frames: 10, used: true }).action, 'practice');
assert.equal(decideAfterWin({ now, validAt: 5000, frames: 10, used: true, pending: pend }).action, 'practice');

// node refusals vs transport trouble
assert.equal(isNodeRefusal(new Error('submit_block: Block not accepted')), true);
assert.equal(isNodeRefusal(new Error('submit_block: HTTP 502')), false);
assert.equal(isNodeRefusal(new Error('timeout')), false);
assert.equal(isNodeRefusal(new TypeError('Failed to fetch')), false);
assert.equal(refusalReason(new Error('submit_block: predates the chain\'s launch')), 'predates the chain\'s launch');

// confirmBlock with a fake clock
const mk = (blocks) => {
  let t = 100; const calls = [];
  return { calls, d: { now: () => t, sleep: async (ms) => { t += ms / 1000; }, blockAt: async (h) => { calls.push(t); const f = blocks(t, h); if (f === 'throw') throw new Error('x'); return f; } } };
};
const tpl = { height: 5, prev_hash: 'aa' };
let r = mk((t) => (t >= 103 ? { prev_hash: 'aa', blob: '00AB' + 'KEY'.toLowerCase() + '00' } : null));
assert.deepEqual(await confirmBlock(r.d, tpl, 'key', 100), { status: 'accepted' });          // appears 3 s in
r = mk((t) => (t < 102 ? 'throw' : { prev_hash: 'aa', blob: 'ffff' }));
assert.equal((await confirmBlock(r.d, tpl, 'key', 100)).status, 'lost');                      // someone else's block
r = mk(() => null);
const miss = await confirmBlock(r.d, tpl, 'key', 100, 5);
assert.equal(miss.status, 'missing'); assert.ok(r.calls[r.calls.length - 1] >= 105 && r.calls[r.calls.length - 1] < 107); // gives up just past due + grace
console.log('mineflow ok');
