// Mining correctness with the LOCAL time limit (lib/limit.js, the Rust rules): a run over the limit is held until the second the
// limit reaches it, and the block stamped then meets the node's limit at that timestamp; a shorter run found while holding is
// valid sooner; a run within the limit is sent at once. The auction hold no longer asks the node for sm64_valid_at.
import assert from 'node:assert/strict';
import { loadLimit } from './limit.testwasm.mjs';
import { limitNow, validAtFor, maxFrames } from './limit.js';
import { decideAfterWin, effectiveValidAt, startNow } from './mineflow.js';
import { holdUntilValid } from './webminer/bestRun.js';

if (!(await loadLimit())) { console.log('mineflow.limit: skipped (perturb wasm not built)'); process.exit(0); }

const parent = { timestamp: 1_800_000_000, difficulty: 12_345_678 };
const F = 6000; // a run that is over the limit at the parent's timestamp
assert.ok(limitNow(parent, parent.timestamp + 1) < F && limitNow(parent, parent.timestamp + 100000) >= F);
const v = validAtFor(parent, F);
assert.ok(v > parent.timestamp + 30, 'a real wait');

// the manual flow's decision: hold before, send at the due second; the stamped block meets the node's limit
assert.equal(decideAfterWin({ now: parent.timestamp + 10, validAt: v, frames: F }).action, 'hold');
assert.equal(decideAfterWin({ now: v - 1, validAt: v, frames: F }).action, 'hold');
assert.equal(decideAfterWin({ now: v, validAt: v, frames: F }).action, 'send');
const stamped = Math.max(v, v);
assert.ok(maxFrames(parent.difficulty, stamped - parent.timestamp) >= F, 'the stamp meets the limit');
assert.ok(maxFrames(parent.difficulty, stamped - 1 - parent.timestamp) < F, 'and not one second earlier: held exactly as long as needed');
// the launch floor of mainnet
assert.equal(effectiveValidAt(validAtFor(parent, F), 1_800_000_000 + 99999), 1_800_000_000 + 99999);

// the auto-miner's hold with a fake clock: released `lead` seconds before the due second, never earlier
{
  let clock = parent.timestamp + 20, best = { frames: F };
  const log = [];
  const r = await holdUntilValid({
    best: () => best,
    validAt: async (frames) => validAtFor(parent, frames),
    tip: async () => 'tip', tipHash: 'tip', lead: 5, current: () => true,
    sleep: async (ms) => { clock += ms / 1000; }, now: () => clock, clock: () => clock * 1000,
    onHeld: (run, at, changed) => log.push([run.frames, at, changed]),
  });
  assert.equal(r.kind, 'valid');
  assert.equal(r.validAt, v);
  assert.ok(clock >= v - 5 - 0.001 && clock < v - 5 + 5.01, `released at ${clock - v} s from due`);
  assert.equal(log[0][0], F);
  assert.ok(maxFrames(parent.difficulty, Math.max(Math.floor(clock), r.validAt) - parent.timestamp) >= F, 'the submitted block (stamped max(now, validAt)) is within the limit');
}
// a shorter run found while holding becomes valid sooner
{
  let clock = parent.timestamp + 20, best = { frames: F }, swapped = false;
  const r = await holdUntilValid({
    best: () => best,
    validAt: async (frames) => validAtFor(parent, frames),
    tip: async () => 'tip', tipHash: 'tip', lead: 5, current: () => true,
    sleep: async (ms) => { clock += ms / 1000; if (!swapped && clock > parent.timestamp + 40) { swapped = true; best = { frames: 3000 }; } },
    now: () => clock, clock: () => clock * 1000, onHeld: () => {},
  });
  assert.equal(r.run.frames, 3000);
  assert.ok(r.validAt < v);
}
// a tip that moves while holding makes the run stale (the tip comes from the store's memory)
{
  let clock = parent.timestamp + 20, tip = 'tip';
  const r = await holdUntilValid({
    best: () => ({ frames: F }), validAt: async (f) => validAtFor(parent, f), tip: async () => tip, tipHash: 'tip', lead: 5, current: () => true,
    sleep: async (ms) => { clock += ms / 1000; if (clock > parent.timestamp + 60) tip = 'new'; },
    now: () => clock, clock: () => clock * 1000, onHeld: () => {},
  });
  assert.equal(r.kind, 'stale');
}
// within the limit already: valid at once (the first second after the parent is the earliest)
{
  const easy = validAtFor(parent, 600);
  assert.equal(easy, parent.timestamp + 1);
  assert.equal(decideAfterWin({ now: parent.timestamp + 50, validAt: easy, frames: 600 }).action, 'send');
}
// the start mode flips at the power-on time with the clock alone
assert.equal(startNow({ sm64_start: 'parent', sm64_power_on_at: parent.timestamp + 3600 }, parent.timestamp + 3599), 'parent');
assert.equal(startNow({ sm64_start: 'parent', sm64_power_on_at: parent.timestamp + 3600 }, parent.timestamp + 3600), 'either');
console.log('mineflow.limit ok');
