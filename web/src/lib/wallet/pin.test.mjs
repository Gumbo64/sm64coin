import assert from 'node:assert/strict';
import { wrapSecret, unwrapSecret, tryPin, validPin, triesLeft, randomPassword, pinFailure, PIN_ITER, PIN_TRIES, PIN_NOTE } from './pin.js';

const FAST = { iter: 1000 }; // the real 600 000 rounds are slow in a test
assert.equal(PIN_ITER, 600000); assert.equal(PIN_TRIES, 5);
assert.match(PIN_NOTE, /convenience lock/);

// the PIN rule
for (const ok of ['1234', '12345678', '0000']) assert.ok(validPin(ok), ok);
for (const bad of ['123', '123456789', '12a4', '', ' 1234', '١٢٣٤', undefined, null]) assert.ok(!validPin(bad), String(bad));
await assert.rejects(wrapSecret('pw', '12', FAST), /4 to 8 digits/);

// wrap / unwrap
const rec = await wrapSecret('correct horse battery', '4821', { ...FAST, generated: true });
assert.equal(rec.generated, true); assert.equal(rec.fails, 0); assert.equal(rec.tries, 5);
assert.ok(!JSON.stringify(rec).includes('correct') && !JSON.stringify(rec).includes('4821'));
assert.equal(await unwrapSecret(rec, '4821'), 'correct horse battery');
assert.equal(await unwrapSecret(rec, '4822'), null);
assert.equal(await unwrapSecret({ ...rec, ct: rec.ct.slice(0, -4) + 'AAAA' }, '4821'), null, 'tampered');
assert.equal(await unwrapSecret(null, '4821'), null);
const rec2 = await wrapSecret('correct horse battery', '4821', FAST);
assert.notEqual(rec.salt, rec2.salt); assert.notEqual(rec.ct, rec2.ct);
assert.equal(await unwrapSecret(rec, '4821'), await unwrapSecret(rec2, '4821'));

// random passwords
const a = randomPassword(), b = randomPassword();
assert.match(a, /^[0-9a-f]{48}$/); assert.notEqual(a, b);

// the attempt limit: five wrong PINs delete the record
{
  let stored = await wrapSecret('pw-for-wallet', '2468', FAST);
  const save = async (r) => { stored = r; };
  for (let left = 4; left >= 1; left--) {
    const r = await tryPin(stored, '0000', save);
    assert.deepEqual([r.ok, r.left, r.wiped], [false, left, false]);
    assert.equal(triesLeft(stored), left);
  }
  const last = await tryPin(stored, '0000', save);
  assert.deepEqual([last.ok, last.left, last.wiped], [false, 0, true]);
  assert.equal(stored, null, 'wiped');
  assert.match(pinFailure(last), /gone/);
  // and afterwards even the right PIN opens nothing
  const after = await tryPin(stored, '2468', save);
  assert.equal(after.ok, false); assert.equal(after.wiped, true);
}

// a right PIN resets the count; the count is saved before the check
{
  let stored = await wrapSecret('pw-for-wallet', '2468', FAST);
  const seen = [];
  const save = async (r) => { seen.push(r && r.fails); stored = r; };
  for (let i = 0; i < 4; i++) await tryPin(stored, '1111', save);
  assert.equal(stored.fails, 4);
  assert.match(pinFailure({ wiped: false, left: 1 }), /1 try left/);
  seen.length = 0;
  const ok = await tryPin(stored, '2468', save);
  assert.deepEqual([ok.ok, ok.secret], [true, 'pw-for-wallet']);
  assert.deepEqual(seen, [5, 0], 'counted first (5), then reset (0)');
  assert.equal(stored.fails, 0);
  for (let i = 0; i < 4; i++) await tryPin(stored, '1111', save);
  assert.notEqual(stored, null, 'four more wrong ones are fine again');
}

// a stored count at the limit with the RIGHT PIN still opens (the last try counts like any other)
{
  let stored = { ...(await wrapSecret('pw', '9999', FAST)), fails: 4 };
  const r = await tryPin(stored, '9999', async (x) => { stored = x; });
  assert.equal(r.ok, true);
}

// a bad shape counts as a try too (no free probing)
{
  let stored = await wrapSecret('pw', '9999', { ...FAST, tries: 3 });
  await tryPin(stored, 'x', async (x) => { stored = x; });
  assert.equal(stored.fails, 1);
}
console.log('pin ok');
