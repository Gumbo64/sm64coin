import assert from 'node:assert/strict';
import { Coverage, groupOf } from './coverage.js';

// The chart compares workers by the step's place (level, area, goal), not the step key (which holds each worker's seed).
assert.equal(groupOf('101:900:24:1:Stars'), '24:1:Stars');
assert.equal(groupOf('7:0:6:1:Enter'), '6:1:Enter');

let clock = 1_000_000;
Date.now = () => clock;
const tick = () => { clock += 1000; };

// Three independent workers (own seeds): workers 1 and 2 on Whomp's Fortress, worker 3 on Bob-omb Battlefield.
const c = new Coverage();
const stepNo = (k) => [4, 4, 5][k];
c.add(0, '11:900:24:1:Stars', { places: 3, records: 0, new: ['a', 'b', 'c'] }, 3, stepNo); tick();
c.add(1, '22:950:24:1:Stars', { places: 2, records: 1, new: ['b', 'd'] }, 3, stepNo); tick();
c.add(2, '33:1200:9:1:Stars', { places: 7, records: 0, new: ['x1', 'x2', 'x3', 'x4', 'x5', 'x6', 'x7'] }, 3, stepNo); tick();
let s = c.series[c.series.length - 1];
// Two of three on WF beat one on BoB (even though BoB's worker is further).
assert.equal(s.key, '24:1:Stars');
assert.equal(s.n, 2);
assert.equal(s.of, 3);
assert.equal(s.label, "Whomp's Fortress");
assert.equal(s.union, 4); // a b c d
assert.deepEqual(s.per, [3, 2, null]); // worker 3's count does not go in
assert.equal(s.rec, 1);
assert.equal(c.overlap(), 4 / 5);

// Every worker's count is there (not only the first worker's).
c.add(1, '22:950:24:1:Stars', { places: 4, records: 1, new: ['e', 'f'] }, 3, stepNo); tick();
s = c.series[c.series.length - 1];
assert.deepEqual(s.per, [3, 4, null]);
assert.equal(s.union, 6);

// Worker 2 moves on to BoB: two on BoB now, the chart follows them (a marker), worker 1 drops out of it.
c.add(1, '23:2000:9:1:Stars', { places: 2, records: 0, new: ['x1', 'y1'] }, 3, (k) => [4, 5, 5][k]); tick();
s = c.series[c.series.length - 1];
assert.equal(s.key, '9:1:Stars');
assert.equal(s.label, 'Bob-omb Battlefield');
assert.deepEqual(s.per, [null, 2, 7]);
assert.equal(s.union, 8); // x1..x7, y1
assert.equal(c.markers.length, 1);
assert.equal(c.markers[0].label, 'Bob-omb Battlefield');

// One worker alone: no overlap.
const one = new Coverage();
one.add(0, '1:0:6:1:Enter', { places: 5, records: 0, new: ['p', 'q'] }, 1); tick();
assert.equal(one.series.length, 1);
assert.equal(one.overlap(), null);

// The strip: kinds are new places / faster routes / no progress / no data; a found run is marked and the slices after it "beat" it.
{
  const b = new Coverage();
  const realNow = Date.now; let clock = 1e6; Date.now = () => clock;
  const at = (s) => { clock = 1e6 + s * 1000; };
  at(0); b.add(0, '1:0:9:1:Stars', { places: 4, records: 0, new: ['a', 'b', 'c', 'd'] }, 1);
  at(5); b.add(0, '1:0:9:1:Stars', { places: 7, records: 0, new: ['e', 'f', 'g'] }, 1);
  at(16); b.add(0, '1:0:9:1:Stars', { places: 7, records: 1, new: [] }, 1);
  at(31); b.add(0, '1:0:9:1:Stars', { places: 4, records: 1, new: [] }, 1);
  at(100); b.runFound();
  at(106); b.add(0, '1:0:9:1:Stars', { places: 4, records: 1, new: [] }, 1);
  Date.now = realNow;
  const k = b.buckets();
  assert.equal(k[0].kind, 'new');
  assert.equal(k[1].kind, 'faster');
  assert.equal(k[2].kind, 'noprogress');
  assert.equal(k[3].kind, 'none'); assert.equal(k[3].beating, false); // no sample in 45..60 s: no data
  assert.equal(b.runs.length, 1);
  assert.equal(k[6].run, true); assert.equal(k[6].beating, true);
  assert.equal(k[5].beating, false);
}
console.log('coverage ok');
