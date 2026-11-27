import assert from 'node:assert/strict';
import { dispatchFrame, unindent, campaignStart, b64decode } from './nativeFrames.js';

const calls = [];
const hub = new Proxy({}, { get: (_, name) => (...a) => calls.push([name, ...a]) });
const hyb = { onMessage: (...a) => calls.push(['hybrid', ...a]) };

dispatchFrame(hub, { w: 0, kind: 'viz', key: '', data: '{"step":{}}' }, hyb);
dispatchFrame(hub, { w: 0, kind: 'state', key: 'k', data: Buffer.from([1, 2, 3, 250]).toString('base64') }, hyb);
dispatchFrame(hub, { w: 0, kind: 'pert', key: 'k', data: '{"tag":"t"}' }, hyb);
dispatchFrame(hub, { w: 0, kind: 'edges', key: 'k', data: Buffer.from([9, 8, 7]).toString('base64') }, hyb);
dispatchFrame(hub, { w: 0, kind: 'path', key: 'k', data: '{"id":5,"flat":[1,2,3]}' }, hyb);
dispatchFrame(hub, { w: 0, kind: 'hybrid_out', key: '', data: '{"kind":"state","id":1,"ok":true,"state":"0aff"}' }, hyb);
dispatchFrame(hub, { w: 0, kind: 'unknown', key: '', data: '' }, hyb);

assert.deepEqual(calls[0], ['ingest', 0, '{"step":{}}']);
assert.deepEqual([...calls[1][3]], [1, 2, 3, 250]);
assert.deepEqual(calls[2], ['setPert', 0, 'k', '{"tag":"t"}']);
assert.deepEqual([...new Uint8Array(calls[3][3])], [9, 8, 7]);
assert.deepEqual([calls[4][0], calls[4][3], [...calls[4][4]]], ['setPath', 5, [1, 2, 3]]);
assert.equal(calls[5][0], 'hybrid');
assert.deepEqual([...calls[5][3]], [10, 255]);
assert.equal(calls[5][2].state, undefined);
assert.equal(calls.length, 6);
assert.equal(unindent('  step 3: Bob at frame 1'), 'step 3: Bob at frame 1');
assert.equal(unindent('    JRB at frame 5 (+1)'), '    JRB at frame 5 (+1)');
assert.equal(unindent('    step 1: castle at frame 0, budget 2.0 M frames'), 'step 1: castle at frame 0, budget 2.0 M frames'); // the anytime search's
assert.equal(campaignStart('  start PowerOn at 0 stars 0 keys, ready at frame 1680 (level 16 castle grounds)'), 'power-on');
assert.equal(campaignStart('    start Parent at 3 stars 0 keys, ready at frame 12 (level 9 BoB)'), 'parent');
assert.equal(campaignStart('  step 1: castle at frame 0'), null);
assert.deepEqual([...b64decode('Zm9v')], [102, 111, 111]);
console.log('nativeFrames ok');
