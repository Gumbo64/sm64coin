import assert from 'node:assert/strict';
import { newPassword, NO_PASSWORD_WARNING } from './pwRule.js';

assert.deepEqual(newPassword('', ''), { ok: true, error: '', warn: NO_PASSWORD_WARNING });
assert.equal(newPassword('a', 'a').ok, false);
assert.match(newPassword('a', 'a').error, /too short/);
assert.match(newPassword('abcdefg', '').error, /too short/);
assert.match(newPassword('abcdefgh', 'abcdefgx').error, /differ/);
assert.match(newPassword('abcdefgh', '').error, /differ/);
assert.match(newPassword('', 'abcdefgh').error, /too short/);   // repeated but not typed: not silently "no password"
assert.deepEqual(newPassword('abcdefgh', 'abcdefgh'), { ok: true, error: '', warn: '' });
console.log('pwRule ok');
