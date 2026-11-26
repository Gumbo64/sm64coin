import assert from 'node:assert/strict';
import { reencryptForMove, exportMinerState, parseMinerState, mergeMinerState, MINER_STATE_FORMAT } from './migrate.js';
import { parseBackup, BACKUP_FORMAT } from './wallet/backup.js';

const store = (init = {}) => { const m = { ...init }; return { getItem: (k) => (k in m ? m[k] : null), setItem: (k, v) => { m[k] = String(v); }, m }; };

// --- miner state round trip
const old = store({ 'sm64coin.address': 'SM6abc', 'sm64coin.signedSeeds': JSON.stringify({ aa11: { print: 'p1', frames: 100, at: 5 }, bb22: { print: 'p2', frames: 90, at: 6 } }) });
const text = exportMinerState(old);
const st = parseMinerState(text);
assert.equal(Object.keys(st.signedSeeds).length, 2);
const neu = store({ 'sm64coin.signedSeeds': JSON.stringify({ aa11: { print: 'LOCAL', frames: 1, at: 9 } }) });
assert.deepEqual(mergeMinerState(neu, st), { added: 1, kept: 1 });
const merged = JSON.parse(neu.m['sm64coin.signedSeeds']);
assert.equal(merged.aa11.print, 'LOCAL', 'an entry already on this origin wins');
assert.equal(merged.bb22.print, 'p2');
assert.equal(neu.m['sm64coin.address'], undefined, 'the payout address is not imported');
assert.throws(() => parseMinerState('{"format":"x"}'), /not an SM64Coin miner state/);
assert.throws(() => parseMinerState('nope'), /not a miner state/);
assert.throws(() => parseMinerState(JSON.stringify({ format: MINER_STATE_FORMAT, version: 2 })), /newer/);
assert.deepEqual(parseMinerState(JSON.stringify({ format: MINER_STATE_FORMAT, version: 1, signedSeeds: { '<script>': { print: 'x', frames: 1, at: 1 }, ok: { print: 'x', frames: 1, at: 1 } } })).signedSeeds, { }, 'bad keys dropped');

// --- wallet re-encrypt with a fake engine whose "keys file" records the password it was made with
const mkWallet = (address, pw) => ({
  address, pw, closed: false,
  getPrimaryAddress: async () => address, getRestoreHeight: async () => 1234, close: async function () { this.closed = true; },
});
const fakeEng = {
  allKeys: async (w) => ({ address: w.address, spendKey: 'ab'.repeat(32), viewKey: 'cd'.repeat(32), seed: 'one two three' }),
  isViewOnly: async () => false,
  restoreFromSpendKey: async (sk, pw, h) => { assert.equal(sk, 'ab'.repeat(32)); assert.equal(h, 1234); return mkWallet('SM6same', pw); },
  restoreViewOnly: async () => { throw new Error('not used'); },
  makeBackup: async (w) => ({ name: 'sm64coin-wallet-SM6same.json', doc: JSON.stringify({ format: BACKUP_FORMAT, version: 1, network: 'mainnet', address: w.address, keysData: Buffer.from('keys:' + w.pw).toString('base64') }) }),
};
const w = mkWallet('SM6same', 'oldpassword');
await assert.rejects(reencryptForMove(fakeEng, w, 'short'), /8\+/);
const r = await reencryptForMove(fakeEng, w, 'new password 1');
assert.equal(r.name, 'sm64coin-wallet-SM6same-moved.json');
assert.equal(r.seed, 'one two three');
const parsed = parseBackup(new TextEncoder().encode(r.doc));
assert.equal(Buffer.from(parsed.keysData).toString(), 'keys:new password 1', 'the file is locked with the NEW password');
assert.equal(parsed.meta.address, 'SM6same');
assert.ok(!r.doc.includes('ab'.repeat(32)) && !r.doc.includes('cd'.repeat(32)), 'no clear keys in the file');
// a wallet that is not re-creatable from its spend key is refused
await assert.rejects(reencryptForMove({ ...fakeEng, restoreFromSpendKey: async (a, pw) => mkWallet('SM6other', pw) }, w, 'new password 1'), /cannot be re-created/);
console.log('migrate tests ok');
