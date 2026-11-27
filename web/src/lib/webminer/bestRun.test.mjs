import assert from 'node:assert/strict';
import { offerRun, holdUntilValid, signedSeeds, runPrint, clampLead, DEFAULT_LEAD, TIP_EVERY } from './bestRun.js';
const SEND_AHEAD = DEFAULT_LEAD;

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
  const w = { lead: undefined, t: 1000, best: { frames: 3000, proof: 'p1' }, asks: [], held: [], tips: 0, tip: 'A', stopped: false };
  const fire = () => { for (const e of events) if (!e.done && w.t >= e.at) { e.done = true; e.run(w); } if (w.t >= tipAt) w.tip = 'B'; };
  w.d = {
    best: () => w.best,
    validAt: async (frames) => { w.asks.push(frames); return limitAt(frames); },
    tip: async () => { w.tips++; return w.tip; },
    tipHash: 'A',
    get lead() { return w.lead; },
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

// The lead is configurable: a 30 s lead releases 30 s before the run is valid; 0 releases at the stamp.
{
  const w = world({ limitAt: () => 1000 + 300 });
  w.lead = 30;
  const h = await holdUntilValid(w.d);
  assert.equal(h.kind, 'valid');
  assert.ok(w.t >= 1300 - 30 && w.t < 1300 - 30 + 6, `released at ${w.t}`);
  const w0 = world({ limitAt: () => 1000 + 300 });
  w0.lead = 0;
  await holdUntilValid(w0.d);
  assert.ok(w0.t >= 1300 && w0.t < 1306, `released at ${w0.t}`);
  assert.equal(clampLead('x'), DEFAULT_LEAD);
  assert.equal(clampLead(-3), 0);
  assert.equal(clampLead(9999), 25);
}

// An equivocated tip: the node builds on an older block, so the template's tip differs from its prev_hash. Only the
// template's tip counts: the tip staying the same is not "a new block", and moving off it is.
{
  const w = world({ limitAt: () => 1000 + 3600, tipAt: Infinity });
  w.d.tipHash = 'T'; // the template's parent is not the tip
  w.tip = 'T';
  w.t = 1000;
  w.d.sleep = async (ms) => { w.t += ms / 1000; if (w.t >= 1200) w.tip = 'U'; };
  const h = await holdUntilValid(w.d);
  assert.equal(h.kind, 'stale', 'noticed only when the tip moved off T');
  assert.ok(w.t >= 1200, `at ${w.t}`);
}

// One run per key: a second, different run for the same seed is refused, the same run is not; a page refresh
// (a new signedSeeds on the same storage) remembers; a throwing storage still guards in memory.
{
  const mem = {};
  const st = { getItem: (k) => mem[k] ?? null, setItem: (k, v) => { mem[k] = v; } };
  const a = signedSeeds(st);
  assert.equal(a.signedOther('seed1', 'aabb'), null);
  a.record('seed1', 'aabb', 3000);
  assert.equal(a.signedOther('seed1', 'aabb'), null, 'the same run again');
  assert.equal(a.signedOther('seed1', 'aabc'), 3000, 'a different run');
  assert.equal(a.signedOther('seed2', 'aabc'), null);
  assert.equal(signedSeeds(st).signedOther('seed1', 'aabc'), 3000, 'after a refresh');
  a.forget('seed1'); // the node refused the submission: nothing was stored, a shorter run may be signed
  assert.equal(a.signedOther('seed1', 'aabc'), null);
  assert.equal(signedSeeds(st).signedOther('seed1', 'aabc'), null, 'forgotten after a refresh too');
  const bad = { getItem() { throw new Error('blocked'); }, setItem() { throw new Error('blocked'); } };
  const b = signedSeeds(bad);
  b.record('s', 'ff', 10);
  assert.equal(b.signedOther('s', 'fe'), 10, 'in memory');
  assert.notEqual(runPrint('aabb'), runPrint('aabc'));
}

console.log('bestRun ok');

// ----- v0.4: wait for the height's proofs, and publish a lost run as a proof -----
import { waitRecordsReady, offerAsProof, proofVerdict, MAX_RECORDS_WAIT } from './bestRun.js';
import { windowFrames, dueElapsed } from '../limit.js';
import { loadLimit } from '../limit.testwasm.mjs';
const limitOk = await loadLimit();

{
  const clock = { t: 1000 };
  const asks = [];
  const d = { fetch: async () => { asks.push(clock.t); return { sm64_records_ready_at: 0, again: true }; }, now: () => clock.t,
    sleep: async (ms) => { clock.t += ms / 1000; }, current: () => true, onWait: (s) => asks.push(`wait ${s}`) };
  const r = await waitRecordsReady({ sm64_records_ready_at: 1040 }, d);
  assert.equal(r.again, true);
  assert.deepEqual(asks, ['wait 40', 1040], 'told once, took the template at the ready time');
  // already ready: the same answer, no request
  assert.deepEqual(await waitRecordsReady({ sm64_records_ready_at: 900, x: 1 }, d), { sm64_records_ready_at: 900, x: 1 });
  assert.equal(asks.length, 2);
  // a node with a far-off clock: bounded
  clock.t = 0;
  await waitRecordsReady({ sm64_records_ready_at: 1e9 }, { ...d, fetch: async () => ({}) });
  assert.equal(clock.t, MAX_RECORDS_WAIT);
  // a newer round (or stop) ends it
  assert.equal(await waitRecordsReady({ sm64_records_ready_at: 1e9 }, { ...d, current: () => false }), null);
}

// the archive window agrees with Rust (docs/V0.4-DESIGN.md: window_frames(1, 3950, 60) = 4,650)
if (limitOk) {
  assert.equal(windowFrames(1, 3950, 60), 4650);
  assert.ok(dueElapsed(1, 3950) > 300 && windowFrames(1, 3950, 0) >= 3950);
} else console.log('bestRun: window checks skipped (perturb wasm not built)');

{
  const mk = (winner, res = { result: 1 }) => {
    const sent = [];
    const signed = signedSeeds(null);
    return { sent, signed, d: { template: { height: 7, sm64_seed_hash: 'seed', sm64_retarget_difficulty: 1 }, run: { proof: 'aabb', frames: 4131 }, signed,
      winnerFrames: async () => winner, proofBytes: (h) => h, sign: async () => 'sig', seal: (p, s) => `${p}|${s}`, submit: async (h) => { sent.push(h); return res; } } };
  };
  let w = mk(3950);
  assert.deepEqual(await offerAsProof(w.d), { kind: 'sent', result: 1, reason: '' });
  assert.deepEqual(w.sent, ['aabb|sig']);
  assert.equal(w.signed.signedOther('seed', 'ccdd'), 4131, 'recorded: one run per key');
  assert.equal((await offerAsProof({ ...w.d, run: { proof: 'ccdd', frames: 4000 } })).kind, 'second', 'never a second run for the seed');
  assert.equal(w.sent.length, 1);
  assert.equal((await offerAsProof(w.d)).kind, 'sent', 'the same run again is the same record');
  w = mk(3950);
  assert.deepEqual(await offerAsProof({ ...w.d, run: { proof: 'aabb', frames: 4782 } }), { kind: 'outside', winner: 3950, limit: 4650 });
  assert.equal(w.sent.length, 0);
  w = mk(null, { result: 0, reason: 'known' });  // winner unknown: the node decides
  assert.deepEqual(await offerAsProof({ ...w.d, run: { proof: 'aabb', frames: 4782 } }), { kind: 'sent', result: 0, reason: 'known' });
  w = mk(3950);
  assert.equal((await offerAsProof({ ...w.d, submit: async () => { throw new Error('boom'); } })).kind, 'error');
  assert.match(proofVerdict(0), /ignored/);
  assert.equal(proofVerdict(1), 'in the pool');
}
console.log('v0.4 miner tests ok');
