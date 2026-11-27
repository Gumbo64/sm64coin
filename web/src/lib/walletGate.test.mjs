import assert from 'node:assert/strict';
import { mineGate, sessionOther, setSessionOther } from './walletGate.js';

assert.equal(mineGate({ walletKnown: undefined }), 'loading');
assert.equal(mineGate({ walletKnown: false }), 'interstitial');                       // no wallet, nothing chosen
assert.equal(mineGate({ walletKnown: false, other: '' }), 'interstitial');
assert.equal(mineGate({ walletKnown: false, other: 'SM6abc' }), 'mine');              // mine for another address
assert.equal(mineGate({ walletKnown: true }), 'mine');                                 // a locked wallet: no interstitial
assert.equal(mineGate({ walletKnown: true, other: 'SM6abc' }), 'mine');
const mem = (() => { const m = {}; return { getItem: (k) => m[k] ?? null, setItem: (k, v) => { m[k] = v; } }; })();
assert.equal(sessionOther(mem), '');
setSessionOther('SM6x', mem);
assert.equal(sessionOther(mem), 'SM6x');
assert.equal(sessionOther({ getItem() { throw new Error('x'); } }), '');
console.log('walletGate ok');
