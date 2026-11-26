// Memory path of the per-block state cache (no IndexedDB in node: the disk part is skipped).
import assert from 'node:assert/strict';
import { getBlockState, putBlockState, forgetBlockStates } from './blockstates.js';

const page = (v) => new Uint8Array(4096).fill(v);
const powerOn = { mem: { top: 3 * 4096, pages: [page(1), page(2), page(3)] }, pert: null };
const st = (v) => ({ mem: { top: 3 * 4096, pages: [powerOn.mem.pages[0], page(v), powerOn.mem.pages[2]] }, pert: null });

assert.equal(await getBlockState(powerOn, 'aa'), null);
await putBlockState(powerOn, 'aa', st(9));
const hit = await getBlockState(powerOn, 'aa');
assert.equal(hit.from, 'memory');
assert.equal(hit.state.mem.pages[1][0], 9);
// bounded LRU: 4 kept, the least recently used goes first
for (const h of ['b', 'c', 'd']) await putBlockState(powerOn, h, st(1));
await getBlockState(powerOn, 'aa'); // touch aa
await putBlockState(powerOn, 'e', st(1)); // evicts b
assert.ok(await getBlockState(powerOn, 'aa'));
assert.equal(await getBlockState(powerOn, 'b'), null);
// another game build (different power-on) does not see these states
const other = { mem: { top: 3 * 4096, pages: [page(7), page(8), page(9)] }, pert: null };
assert.equal(await getBlockState(other, 'aa'), null);
forgetBlockStates();
assert.equal(await getBlockState(powerOn, 'aa'), null);
console.log('blockstates ok');
