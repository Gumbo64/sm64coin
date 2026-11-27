import assert from 'node:assert/strict';
import { secretFrom, bioMessage, dropQuick } from './quickunlock.js';
import { wrapSecret, triesLeft } from './pin.js';

const PW = 'the-wallet-password';
const check = async (x) => { if (x !== PW) throw new Error('Wrong password.'); };

// PIN: the password still works; a PIN opens the sealed password; wrong PINs are counted and the fifth wipes
{
  let quick = { kind: 'pin', ...(await wrapSecret(PW, '2468', { iter: 1000 })) };
  const setQuick = async (r) => { quick = r; };
  assert.equal(await secretFrom({ input: PW, quick, setQuick, check }), PW, 'the password itself');
  assert.equal(quick.fails, 0, 'a password does not cost a PIN try');
  assert.equal(await secretFrom({ input: '2468', quick, setQuick, check }), PW, 'the PIN');
  await assert.rejects(secretFrom({ input: 'nope-nope', quick, setQuick, check }), /Wrong password/, 'not a PIN shape: the password error, no try used');
  assert.equal(quick.fails, 0);
  for (let i = 4; i >= 1; i--) {
    await assert.rejects(secretFrom({ input: '1111', quick, setQuick, check }), new RegExp(`${i} ${i === 1 ? 'try' : 'tries'} left`));
    assert.equal(triesLeft(quick), i);
  }
  await assert.rejects(secretFrom({ input: '1111', quick, setQuick, check }), /gone/);
  assert.equal(quick, null, 'wiped');
  await assert.rejects(secretFrom({ input: '2468', quick, setQuick, check }), /Wrong password/, 'after the wipe only the password opens');
  assert.equal(await secretFrom({ input: PW, quick, setQuick, check }), PW);
}

// a PIN whose sealed password no longer matches the wallet
{
  let quick = { kind: 'pin', ...(await wrapSecret('old-password', '2468', { iter: 1000 })) };
  await assert.rejects(secretFrom({ input: '2468', quick, setQuick: async (r) => { quick = r; }, check }), /no longer matches/);
}

// fingerprint (Android): the Keystore bridge
{
  const calls = [];
  const mk = (impl) => ({ unwrap: async (b) => { calls.push(['unwrap', b]); return impl(); }, forget: async () => { calls.push(['forget']); } });
  let quick = { kind: 'android', blob: 'BLOB', generated: false };
  const setQuick = async (r) => { quick = r; };
  assert.equal(await secretFrom({ quick, setQuick, bio: mk(() => PW), check }), PW);
  assert.deepEqual(calls[0], ['unwrap', 'BLOB']);
  // typed password still works with a fingerprint set (no prompt)
  calls.length = 0;
  assert.equal(await secretFrom({ input: PW, quick, setQuick, bio: mk(() => { throw new Error('no'); }), check }), PW);
  assert.equal(calls.length, 0);
  // cancelled: no message
  const cancelled = await secretFrom({ quick, setQuick, bio: mk(() => { const e = new Error('x'); e.code = 'cancelled'; throw e; }), check }).catch((e) => e);
  assert.equal(cancelled.cancelled, true);
  // invalidated (new fingerprint enrolled): quick-unlock is dropped, the key forgotten
  const inv = await secretFrom({ quick, setQuick, bio: mk(() => { const e = new Error('x'); e.code = 'invalidated'; throw e; }), check }).catch((e) => e);
  assert.match(inv.message, /turned off/);
  assert.equal(quick, null);
  assert.ok(calls.some((c) => c[0] === 'forget'));
  // a blob for another password
  quick = { kind: 'android', blob: 'B' };
  await assert.rejects(secretFrom({ quick, setQuick, bio: mk(() => 'other'), check }), /no longer matches/);
}
assert.equal(bioMessage({ code: 'cancelled' }), '');
assert.match(bioMessage({ code: 'lockout' }), /wait/);
assert.match(bioMessage({ code: 'quick_failed', message: 'boom' }), /boom/);
let q = { kind: 'pin' }; await dropQuick(q, async (r) => { q = r; }, null); assert.equal(q, null);
console.log('quickunlock ok');
