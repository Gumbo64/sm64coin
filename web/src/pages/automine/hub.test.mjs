import assert from 'node:assert/strict';
globalThis.localStorage ??= { getItem: () => null, setItem: () => {} };
const { VizHub } = await import('./hub.js');

// Visualisations switched off and on again mid-round: the round's workers are back at once, where their campaigns are.
const h = new VizHub();
h.setEnabled(true);
const template = { height: 5 };
h.newRound({ template, starts: ['parent', 'power-on'], parentRaw: null });
assert.equal(h.workers.length, 2);
h.log(0, 'step 1: castle at frame 0, 0 stars 0 keys, budget 2.0 M frames');
h.setEnabled(false);
assert.equal(h.workers.length, 0);
h.log(1, 'step 3: bob at frame 900, 1 stars 0 keys, budget 4.0 M frames'); // while off: only the campaign position
h.restartWorker(0, 'power-on');
h.setEnabled(true);
assert.equal(h.workers.length, 2);
assert.equal(h.template, template);
assert.deepEqual(h.workers.map((w) => w.start), ['power-on', 'power-on']);
assert.equal(h.workers[1].stepNo, 3);
assert.equal(h.workers[1].played, 900);
assert.equal(h.workers[1].starsBase, 1);
assert.equal(h.workers[1].steps.length, 1);
assert.equal(h.workers[0].stepNo, 0); // restarted
// A viz line for the current step after the switch-on goes in (a new step for the views).
h.ingest(1, JSON.stringify({ step: { frame0: 900, seed: 1, goal: 'Stars', level: 9, area: 1, pos: [0, 0, 0] }, t: 1, nodes_total: 1, sim_frames: 10, nodes: [] }));
assert.ok(h.workers[1].cur);
// Toggling again doesn't duplicate the step list (the views get copies).
h.setEnabled(false);
h.setEnabled(true);
assert.equal(h.workers[1].steps.length, 1);
// After the miner stops there is nothing to show.
h.setEnabled(false);
h.roster = null;
h.setEnabled(true);
assert.equal(h.workers.length, 0);
console.log('hub ok');
