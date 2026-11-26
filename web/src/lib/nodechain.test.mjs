import assert from 'node:assert/strict';
import { probeChain, chainNote, GENESIS } from './nodechain.js';

const G = 'cbd56c9bf4ea17392ccfdb7a661f0e3e449b55b95a9a2e1eeea426132dfbde9f';
assert.equal(GENESIS, G);
const node = (hash) => async (m, p) => {
  assert.equal(m, 'get_block_header_by_height');
  assert.deepEqual(p, { height: 0 });
  return { block_header: { hash } };
};
assert.deepEqual(await probeChain(node(G)), { state: 'ok', genesis: G });
assert.equal((await probeChain(node(G.toUpperCase()))).state, 'ok');
// an old v0.2 node: answers, other genesis
const old = await probeChain(node('aa'.repeat(32)));
assert.equal(old.state, 'wrong-chain');
// not answering / garbage
assert.equal((await probeChain(async () => { throw new Error('down'); })).state, 'down');
assert.equal((await probeChain(async () => ({}))).state, 'down');
assert.equal((await probeChain(node('bb'), 'any')).state, 'ok');

assert.match(chainNote({ fallbackFrom: 'wrong-chain', publicNode: 'https://eboy.au' }), /old chain \(v0\.2\): using https:\/\/eboy\.au.*update your node/i);
assert.match(chainNote({ chosen: true, node: 'http://x:1', probe: 'wrong-chain' }), /http:\/\/x:1.*different chain/);
assert.equal(chainNote({ chosen: true, node: 'http://x:1', probe: 'ok' }), '');
assert.equal(chainNote({ chosen: false, probe: 'down' }), '');
console.log('nodechain ok');
