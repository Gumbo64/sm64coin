import assert from 'node:assert/strict';
import { decideAfterWin, effectiveValidAt, launchFor, refusedBeforeClaim, refusalReason, confirmBlock, advanceAfter, nextStep, LAUNCH_TIMESTAMP } from './mineflow.js';

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

// Which refusals free the template again: only those the node gives before it records the claim.
const E = (m) => new Error('submit_block: ' + m);
for (const m of ['Wrong param', 'Wrong block blob', 'Block bloc size is too big, rejecting block', 'Unknown parent block', 'Block is too old for restricted RPC',
  'Busy: this address sent an invalid proof recently, retry later', 'Block not accepted: stamped 5, before the chain\'s launch at 1795651200 (unix seconds); stamp it at or after the launch']) {
  assert.equal(refusedBeforeClaim(E(m)), true, m);
}
// after the claim is recorded (or not provably before it): the key stays spent
for (const m of ['Block not accepted', 'Block not accepted: the block failed verification (an invalid or unsigned proof, a timestamp outside the rules, or a bad miner transaction; the node\'s log says which)',
  'Block not accepted: a different run was already submitted for this template (one run per key): not accepted',
  'Block not accepted: submitted too early: the run is due in 90 s; submit at most 30 s before it is valid',
  'Block not accepted: not queued: the queue is full', 'Block not accepted: already seen and decided by this node\'s replay queue (not queued again)',
  'Block not accepted: its parent is unknown to this node (orphan)', 'Block not accepted: the node already has this block']) {
  assert.equal(refusedBeforeClaim(E(m)), false, m);
}
// transport trouble: the block may have arrived
assert.equal(refusedBeforeClaim(new Error('submit_block: HTTP 502')), false);
assert.equal(refusedBeforeClaim(new Error('timeout')), false);
assert.equal(refusedBeforeClaim(new TypeError('Failed to fetch')), false);
assert.equal(refusedBeforeClaim(new Error('get_info: Wrong param')), false);
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

// auto-advance: only after this template's run is used, and a block landed or the node answered
assert.equal(advanceAfter({ used: true, confirmed: 'accepted' }), true);
assert.equal(advanceAfter({ used: true, tipChanged: true }), true);
assert.equal(advanceAfter({ used: true, confirmed: 'lost' }), true);
assert.equal(advanceAfter({ used: true }), false);                       // sent, not seen yet: practice
assert.equal(advanceAfter({ used: false, tipChanged: true }), false);    // unused template: the restart path handles a new tip
assert.equal(advanceAfter({ used: false, confirmed: 'accepted' }), false);
// a run valid exactly now is sent, one second ahead is held (countdown), a shorter run replaces it
assert.equal(decideAfterWin({ now: 1000, validAt: 1000, frames: 5 }).action, 'send');
assert.equal(decideAfterWin({ now: 1000, validAt: 1001, frames: 5 }).action, 'hold');

// the three transitions after a finished run
assert.equal(nextStep({ held: true }), 'redo');                                     // held: redo for a shorter run
assert.equal(nextStep({ used: true, confirmed: 'accepted' }), 'advance');          // accepted: on to the next block
assert.equal(nextStep({ used: true, confirmed: 'lost' }), 'advance');              // beaten: on too
assert.equal(nextStep({ used: true, tipChanged: true }), 'advance');
assert.equal(nextStep({ used: true }), 'wait');                                    // sent, no answer yet
assert.equal(nextStep({ held: true, used: true, confirmed: 'accepted' }), 'advance'); // a held run that was sent meanwhile
