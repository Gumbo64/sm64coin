import assert from 'node:assert/strict';
import { runsAt, groupRuns, limitShift, tailFrame, replayOptions, recordRuns } from './blockRuns.js';
import { maxFrames } from './rules.js';

const blk = (hash, ts, frames, extra = {}) => ({ block_header: { hash, height: 7, prev_hash: 'p', timestamp: ts }, sm64_frames: frames, miner_tx_hash: 'm', ...extra });
const w = blk('w', 1000, 900);
// winner only
let r = runsAt(w, 400);
assert.deepEqual(r.map((x) => [x.t, x.frames, x.winner]), [[600, 900, true]]);
// siblings: same height and parent only; the winner itself and other heights are dropped
r = runsAt(w, 400, [blk('a', 1100, 1200), blk('w', 1000, 900), { ...blk('z', 1, 1), block_header: { hash: 'z', height: 8, prev_hash: 'p', timestamp: 1 } }, blk('y', 5, 5, { block_header: { hash: 'y', height: 7, prev_hash: 'other', timestamp: 5 } })]);
assert.deepEqual(r.map((x) => x.id), ['w:w', 'a:a']);
assert.equal(r[1].t, 700);
// v0.4 records: stamps -> t, bad rows skipped, non-arrays ignored
const rec = runsAt({ ...w, sm64_records: [{ timestamp: 1200, frames: 1500, hash: 'h1' }, { t: 650, frames: 901 }, { frames: 3 }, null, { timestamp: 'x', frames: 1 }] }, 400);
assert.deepEqual(rec.map((x) => [x.t, x.frames, x.source]), [[600, 900, 'chain'], [800, 1500, 'record'], [650, 901, 'record']]);
assert.deepEqual(recordRuns({ sm64_records_bytes: 'abcd' }, 1), []);
// negative gap clamps to 0
assert.equal(runsAt(blk('w', 300, 5), 400)[0].t, 0);
// grouping: shared dot lists everyone, winner flag carried
const g = groupRuns([{ t: 10, frames: 5, winner: true }, { t: 10.2, frames: 5 }, { t: 11, frames: 6 }]);
assert.equal(g.length, 2); assert.equal(g[0].runs.length, 2); assert.ok(g[0].winner); assert.ok(!g[1].winner);
// limit shift: 0 for a normal run, the restart delay for a shifted one
const d = 20;
assert.equal(limitShift(d, 500, maxFrames(d, 500)), 0);
assert.equal(limitShift(d, 14900, maxFrames(d, 500)), 14400);
assert.equal(limitShift(d, 500, 0), 0);
// tail
assert.equal(tailFrame(10000, 900, 10), 9700);
assert.equal(tailFrame(10000, 200, 10), 9800);
assert.equal(tailFrame(10000, 100, 10), 9900);
// options
const win = r[0];
let o = replayOptions(win, { cycleEnd: 10000, hasPads: true });
assert.deepEqual(o.map((x) => x.key), ['start', 'tail5', 'tail10', 'm64']);
assert.equal(o[3].download, 'cycle'); // the one .m64: the cycle from power-on to the run
assert.equal(o[1].to, '/block/w?replay&f=9850');
assert.ok(o.every((x) => !x.disabled));
o = replayOptions(win, { cycleEnd: null, hasPads: false });
assert.ok(o.every((x) => x.disabled));
assert.equal(replayOptions({ ...win, frames: 151 }, { cycleEnd: 1, hasPads: true }).some((x) => x.key === 'tail5'), true);
assert.equal(replayOptions({ ...win, frames: 150 }, { cycleEnd: 1000, hasPads: true }).some((x) => x.key === 'tail5'), false);
assert.equal(replayOptions(r[1], {})[0].to, '/block/a');
assert.ok(replayOptions({ source: 'record', frames: 1, t: 1 }, {})[0].disabled);
console.log('blockRuns ok');
