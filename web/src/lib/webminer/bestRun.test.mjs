import assert from 'node:assert/strict';
import { offerRun, holdUntilValid, SEND_AHEAD, TIP_EVERY } from './bestRun.js';

// offerRun: only a strictly shorter run replaces the best.
assert.deepEqual(offerRun(null, { frames: 3000 }), { best: { frames: 3000 }, kind: 'first' });
const b = { frames: 3000, proof: 'p1' };
assert.equal(offerRun(b, { frames: 3000 }).kind, 'ignored');
assert.equal(offerRun(b, { frames: 3000 }).best, b);
assert.equal(offerRun(b, { frames: 3100 }).kind, 'ignored');
assert.equal(offerRun(b, { frames: 2999 }).kind, 'shorter');
assert.equal(offerRun(b, { frames: 2999 }).best.frames, 2999);

// A fake clock: sleep() advances it and runs the scheduled events (a worker's run arriving, a new tip).
function world({ limitAt, tipAt = Infinity, events = [] }) {
  const w = { t: 1000, best: { frames: 3000, proof: 'p1' }, asks: [], held: [], tips: 0, tip: 'A', stopped: false };
  const fire = () => { for (const e of events) if (!e.done && w.t >= e.at) { e.done = true; e.run(w); } if (w.t >= tipAt) w.tip = 'B'; };
  w.d = {
    best: () => w.best,
    validAt: async (frames) => { w.asks.push(frames); return limitAt(frames); },
    tip: async () => { w.tips++; return w.tip; },
    prevHash: 'A',
    current: () => !w.stopped,
    sleep: async (ms) => { w.t += ms / 1000; fire(); },
    now: () => w.t,
    clock: () => w.t * 1000,
    onHeld: (run, v, changed) => { if (changed) w.held.push([run.frames, v]); },
  };
  return w;
}

// (1) held until valid, (5) then the run to submit, once.
{
  const w = world({ limitAt: () => 1000 + 300 });
  const h = await holdUntilValid(w.d);
  assert.equal(h.kind, 'valid');
  assert.equal(h.run.proof, 'p1');
  assert.ok(w.t >= 1300 - SEND_AHEAD && w.t < 1300 - SEND_AHEAD + 6, `released at ${w.t}`);
  assert.deepEqual(w.asks, [3000]);
  assert.deepEqual(w.held, [[3000, 1300]]);
}

// Valid at once (inside the send-ahead window): not held at all.
{
  const w = world({ limitAt: () => 1000 + SEND_AHEAD });
  const h = await holdUntilValid(w.d);
  assert.equal(h.kind, 'valid');
  assert.equal(w.held.length, 0);
}

// (2) a shorter run arrives while held: it replaces the held one and its validity is asked again (valid sooner);
// (3) a longer/equal run is ignored (offerRun) and does not change what is held.
{
  const limit = (f) => (f >= 3000 ? 1000 + 900 : 1000 + 250); // the shorter run is valid much sooner
  const w = world({
    limitAt: limit,
    events: [
      { at: 1020, run: (w) => { const o = offerRun(w.best, { frames: 3000, proof: 'eq' }); assert.equal(o.kind, 'ignored'); w.best = o.best; } },
      { at: 1030, run: (w) => { const o = offerRun(w.best, { frames: 3200, proof: 'long' }); assert.equal(o.kind, 'ignored'); w.best = o.best; } },
      { at: 1040, run: (w) => { const o = offerRun(w.best, { frames: 2900, proof: 'p2' }); assert.equal(o.kind, 'shorter'); w.best = o.best; } },
    ],
  });
  const h = await holdUntilValid(w.d);
  assert.equal(h.kind, 'valid');
  assert.equal(h.run.proof, 'p2');
  assert.equal(h.validAt, 1250);
  assert.deepEqual(w.asks, [3000, 2900]); // asked again only for the shorter run
  assert.deepEqual(w.held.map((x) => x[0]), [3000, 2900]);
  assert.ok(w.t < 1000 + 900 - SEND_AHEAD, 'released by the shorter run, before the first run would have been');
}

// A shorter run that is valid immediately releases at once.
{
  const w = world({ limitAt: (f) => (f >= 3000 ? 1000 + 900 : 0), events: [{ at: 1010, run: (w) => { w.best = offerRun(w.best, { frames: 2500, proof: 'p3' }).best; } }] });
  const h = await holdUntilValid(w.d);
  assert.equal(h.run.proof, 'p3');
  assert.ok(w.t <= 1015, `released at ${w.t}`);
}

// (4) a new tip while held: stale (the held run is dropped), found by the tip check every TIP_EVERY ms.
{
  const w = world({ limitAt: () => 1000 + 3600, tipAt: 1100 });
  const h = await holdUntilValid(w.d);
  assert.equal(h.kind, 'stale');
  assert.ok(w.t >= 1100 && w.t <= 1100 + TIP_EVERY / 1000 + 5, `noticed at ${w.t}`);
}

// The template moved on (the watch restarted the round) or the miner stopped: gone, nothing submitted.
{
  const w = world({ limitAt: () => 1000 + 3600, events: [{ at: 1050, run: (w) => { w.stopped = true; } }] });
  assert.equal((await holdUntilValid(w.d)).kind, 'gone');
}

// The node can't say (error): the node decides, i.e. submit now.
{
  const w = world({ limitAt: () => { throw new Error('rpc'); } });
  const h = await holdUntilValid(w.d);
  assert.equal(h.kind, 'valid');
  assert.equal(h.validAt, 0);
}

console.log('bestRun ok');
