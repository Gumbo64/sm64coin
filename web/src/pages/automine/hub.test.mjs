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

// The reel replays through the step's perturbation: a clip and a node play carry the step's root state AND its perturbation,
// and a node's recorded position is there for the replay's consistency check.
{
  const g = new VizHub();
  g.setEnabled(true);
  g.newRound({ template: { height: 1 }, starts: ['power-on'], parentRaw: null });
  g.log(0, 'step 1: castle at frame 0, 0 stars 0 keys, budget 2.0 M frames');
  const line = (nodes, best) => JSON.stringify({ step: { frame0: 0, seed: 7, goal: 'Stars', level: 6, area: 1, pos: [1, 2, 3] }, t: 1, nodes_total: 9, sim_frames: 10, key: 'k1', nodes, ...(best ? { best } : {}) });
  g.ingest(0, line([[1, 0, 15, 10, 20, 30, 0, 6, 1, 0]], { id: 1, frame: 15, stars: 0, keys: 0, pos: [10, 20, 30] }));
  g.setRoot(0, g.workers[0].cur.key, new Uint8Array([1, 2, 3]));
  g.setPert(0, g.workers[0].cur.key, JSON.stringify({ tag: 't', pert: '{"x":1}' }));
  assert.equal(g.workers[0].best, null, 'no progress: the first node is not a "best path"');
  assert.deepEqual(g.nodePos(0, g.workers[0].cur.n, 1), [10, 20, 30]);
  assert.deepEqual(g.nodePos(0, g.workers[0].cur.n, 0), [1, 2, 3]); // the step's root: the first cloud point
  assert.equal(g.nodePos(0, g.workers[0].cur.n, 99), null);
  g.mirrors[0] = { path: () => new Uint8Array(8) };
  const p = g.nodePlay(0, g.workers[0].cur.n, 1);
  assert.equal(p.rootRaw.length, 3);
  assert.equal(p.rootPert.pert, '{"x":1}');
  assert.equal(g.clipPlay({ key: 'k', played: 0, rootRaw: null, rootPert: { pert: 'y' }, raw: new Uint8Array(4) }).rootPert.pert, 'y');
  // A star makes a best path.
  g.ingest(0, line([], { id: 1, frame: 15, stars: 1, keys: 0, pos: [10, 20, 30] }));
  assert.equal(g.workers[0].best.stars, 1);
}
console.log('hub ok');

// Objective kinds: a per-step table sent when it grows, nodes carry an index; nodes of older workers (no 11th field) get none.
{
  const { NO_KIND } = await import('./hub.js');
  const g = new VizHub();
  g.setEnabled(true);
  g.newRound({ template: { height: 1 }, starts: ['power-on', 'power-on'], parentRaw: null });
  const line = (nodes, kinds) => JSON.stringify({ step: { frame0: 0, seed: 7, goal: 'Stars', level: 6, area: 1, pos: [1, 2, 3] }, t: 1, nodes_total: 9, sim_frames: 10, nodes, ...(kinds ? { kinds } : {}) });
  g.ingest(0, line([[1, 0, 15, 1, 2, 3, 0, 6, 1, 0, 0], [2, 1, 20, 1, 2, 3, 0, 6, 1, 0, 1]], ['Mario::PosTime', 'Mario::UserGoal']));
  g.ingest(0, line([[3, 2, 30, 1, 2, 3, 0, 6, 1, 0, 1]])); // no new table: the step's earlier one holds
  g.ingest(1, line([[1, 0, 15, 1, 2, 3, 0, 6, 1, 0, 0]], ['Mario::UserGoal'])); // another worker's own indices map onto the shared table
  g.ingest(1, line([[2, 1, 16, 1, 2, 3, 0, 6, 1, 0]])); // an old worker's node: no kind
  const of = (w, id) => { const s = g.index.get((w * 4096 + (g.workers[w].cur.n & 4095)) * 1e8 + id); return g.kindNames[g.kind[s]] ?? (g.kind[s] === NO_KIND ? null : '?'); };
  assert.deepEqual(g.kindNames, ['Mario::PosTime', 'Mario::UserGoal']);
  assert.equal(of(0, 1), 'Mario::PosTime');
  assert.equal(of(0, 3), 'Mario::UserGoal');
  assert.equal(of(1, 1), 'Mario::UserGoal');
  assert.equal(of(1, 2), null);
  assert.equal(of(0, 0), null);
}

// A worker's campaign restarts (a reset cycle to power-on, a relaunch to beat a found run, a template refreshed for the same
// tip): its old tree leaves the cloud; the other workers' nodes stay, with their parents, in order.
{
  const { CAP } = await import('./hub.js');
  const h = new VizHub();
  h.setEnabled(true);
  h.newRound({ template: { height: 7 }, starts: ['parent', 'parent'], parentRaw: null });
  const line = (seed, nodes) => JSON.stringify({ step: { frame0: 0, seed, goal: 'Stars', level: 16, area: 1, pos: [0, 0, 0] }, t: 1, nodes_total: nodes.length, sim_frames: 10, nodes });
  // node: [id, parent, frame, x, y, z, stars, level, area, flags]
  h.ingest(0, line(1, [[1, 0, 5, 10, 0, 0, 0, 16, 1, 0], [2, 1, 9, 20, 0, 0, 0, 16, 1, 0]]));
  h.ingest(1, line(2, [[1, 0, 5, 30, 0, 0, 0, 16, 1, 0], [2, 1, 9, 40, 0, 0, 0, 16, 1, 0]]));
  assert.equal(h.count(), 6); // two roots + four nodes
  const w1 = h.slotOf(1, h.workers[1].cur.n, 2);
  h.selected = { slot: w1 };
  h.restartWorker(0, 'power-on');
  assert.equal(h.count(), 3);
  for (let s = 0; s < h.count(); s++) assert.equal(h.worker[s], 1);
  const n1 = h.workers[1].cur.n;
  const s2 = h.slotOf(1, n1, 2), s1 = h.slotOf(1, n1, 1), s0 = h.slotOf(1, n1, 0);
  assert.deepEqual([s0, s1, s2], [0, 1, 2]);
  assert.equal(h.parentSlot[s2], s1);
  assert.equal(h.parentSlot[s1], s0);
  assert.equal(h.pos[s2 * 3], 40);
  assert.equal(h.selected.slot, s2);
  assert.equal(h.workers[0].start, 'power-on');
  assert.equal(h.workers[0].cur, null);
  // The restarted worker's new tree goes in after them; a node selected on the dropped tree is unselected.
  h.ingest(0, line(3, [[1, 0, 4, 50, 0, 0, 0, 16, 1, 0]]));
  assert.equal(h.count(), 5);
  const s3 = h.slotOf(0, h.workers[0].cur.n, 1);
  assert.equal(s3, 4);
  assert.equal(h.parentSlot[s3], 3);
  h.selected = { slot: s3 };
  h.restartWorker(0, 'parent');
  assert.equal(h.selected, null);
  assert.equal(h.count(), 3);
  // A wrapped ring (more nodes than CAP): the newest CAP minus the dropped worker's, oldest first.
  const r = new VizHub();
  r.setEnabled(true);
  r.newRound({ template: { height: 8 }, starts: ['parent', 'parent'], parentRaw: null });
  for (let k = 0; k < CAP + 10; k++) r.add(k % 2, 1, k + 1, -1, k, [k, 0, 0], 0, 16, 1, 0);
  r.restartWorker(1, 'parent');
  assert.equal(r.count(), CAP / 2);
  assert.equal(r.frame[0], 10); // the oldest kept: node 11 (frame 10), worker 0
  assert.equal(r.frame[r.count() - 1], CAP + 8);
  assert.equal(r.slotOf(0, 1, CAP + 9), r.count() - 1);
  assert.equal(r.slotOf(0, 1, 1), -1); // overwritten long ago
}
