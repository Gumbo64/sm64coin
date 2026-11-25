import assert from 'node:assert/strict';
import { TreeMirror } from './treeMirror.js';

// edge: id, parent, runs [[count, button, sx, sy]]
const edge = (id, parent, runs) => {
  const b = new Uint8Array(10 + runs.length * 5);
  const dv = new DataView(b.buffer);
  dv.setUint32(0, id, true); dv.setUint32(4, parent < 0 ? 0xffffffff : parent, true); dv.setUint16(8, runs.length, true);
  runs.forEach(([n, btn, sx, sy], i) => { b[10 + i * 5] = n; dv.setUint16(11 + i * 5, btn, true); b[13 + i * 5] = sx & 0xff; b[14 + i * 5] = sy & 0xff; });
  return b;
};
const cat = (...a) => { const o = new Uint8Array(a.reduce((n, x) => n + x.length, 0)); let i = 0; for (const x of a) { o.set(x, i); i += x.length; } return o; };

const m = new TreeMirror();
m.add('k', cat(edge(3, 0, [[2, 0x8000, 0, 0]]), edge(7, 3, [[1, 0, 127, -128], [2, 1, 5, 6]])));
m.add('k', edge(9, 7, [[1, 0x4000, -1, 1]]));
assert.equal(m.size, 3);
assert.equal(m.frames('k', 9), 6);
const raw = m.path('k', 9);
assert.equal(raw.length, 24);
const px = (i) => [raw[i * 4] | (raw[i * 4 + 1] << 8), (raw[i * 4 + 2] << 24) >> 24, (raw[i * 4 + 3] << 24) >> 24];
assert.deepEqual(px(0), [0x8000, 0, 0]);
assert.deepEqual(px(1), [0x8000, 0, 0]);
assert.deepEqual(px(2), [0, 127, -128]);
assert.deepEqual(px(4), [1, 5, 6]);
assert.deepEqual(px(5), [0x4000, -1, 1]);
assert.equal(m.path('k', 4), null); // unknown node
assert.equal(m.path('other', 9), null);
assert.equal(m.path('k', 0).length, 0);
m.add('k2', edge(1, 0, [[1, 0, 0, 0]]));
assert.equal(m.size, 1); // a new step drops the old one
assert.equal(m.path('k', 9), null);
console.log('treeMirror ok');
