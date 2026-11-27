// Mining correctness with the LOCAL time limit (lib/limit.js, the Rust rules): a run over the limit is held until the second the
// limit reaches it, and the block stamped then meets the node's limit at that timestamp; a shorter run found while holding is
// valid sooner; a run within the limit is sent at once. The auction hold no longer asks the node for sm64_valid_at.
import assert from 'node:assert/strict';
import { loadLimit } from './limit.testwasm.mjs';
import { limitNow, validAtFor, maxFrames, restartDelayFor } from './limit.js';
import { createTipStore } from './tip.js';
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

// a reset cycle (power-on start on a parent that did not end its cycle) is judged against the limit sm64_restart_delay s earlier
// (sm64pow chain.rs): valid that much later, and the stamp then meets the restart's limit
{
  const delay = 4 * 3600;
  const t = { sm64_start: 'either', sm64_restart_delay: delay, sm64_power_on_at: parent.timestamp + delay };
  assert.equal(restartDelayFor(t, 'power-on'), delay);
  assert.equal(restartDelayFor(t, 'parent'), 0);
  assert.equal(restartDelayFor({ sm64_start: 'power-on', sm64_restart_delay: 0 }, 'power-on'), 0, 'a cycle start is no restart');
  const vr = validAtFor(parent, F, 0, delay);
  assert.equal(vr, v + delay);
  assert.ok(maxFrames(parent.difficulty, vr - parent.timestamp - delay) >= F && maxFrames(parent.difficulty, vr - 1 - parent.timestamp - delay) < F);
  // a short restart run: valid at the power-on time, not before
  assert.equal(validAtFor(parent, 600, 0, delay), parent.timestamp + delay);
}

// this device's clock is off: the held run is still released `lead` s before the NODE's due second (the clock from pushed tips)
for (const skew of [-90, -40, 40, 90]) {
  let local = (parent.timestamp + 20 + skew) * 1000; // our clock, ms; the node's is local - skew
  let handlers = null;
  const store = createTipStore({ now: () => local, setTimeout: () => 0, clearTimeout: () => {}, info: async () => { throw new Error('no poll'); }, header: async () => 0,
    sources: [async (h) => { handlers = h; return () => {}; }] });
  store.subscribe(() => {});
  await new Promise((r) => setTimeout(r, 0));
  handlers.tip({ height: 1, hash: 'tip', timestamp: parent.timestamp, difficulty: String(parent.difficulty), cumulative_difficulty: '1', server_time: Math.floor(local / 1000) - skew });
  const r = await holdUntilValid({
    best: () => ({ frames: F }), validAt: async (f) => validAtFor(parent, f), tip: async () => 'tip', tipHash: 'tip', lead: 5, current: () => true,
    sleep: async (ms) => { local += ms; }, now: () => store.now() / 1000, clock: () => local, onHeld: () => {},
  });
  const nodeAt = local / 1000 - skew;
  assert.equal(r.kind, 'valid');
  assert.ok(nodeAt >= v - 5 - 1 && nodeAt <= v - 5 + 5.5, `skew ${skew}: released at node time ${nodeAt - v} s from due`);
  assert.ok(v - nodeAt < 30, 'inside the node\'s 30 s submit window (RPC_SUBMIT_AHEAD_SECONDS)');
}
console.log('mineflow.limit ok');
