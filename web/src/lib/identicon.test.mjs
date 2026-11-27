import assert from 'node:assert/strict';
import { identicon, identiconSvg, groupedShort } from './identicon.js';

const A = 'SM6' + 'a'.repeat(92), B = 'SM6' + 'a'.repeat(91) + 'b';
assert.equal(identiconSvg(A), identiconSvg(A));            // deterministic
assert.notEqual(identiconSvg(A), identiconSvg(B));         // one character apart: another picture
const i = identicon(A);
for (const row of i.cells) { assert.equal(row.length, 5); for (let c = 0; c < 2; c++) assert.equal(row[c], row[4 - c]); } // mirrored
assert.ok(i.hue >= 0 && i.hue < 360);
// fixed vectors (keccak256 of the address string; bits 0..14 -> 3 mirrored columns, bytes 2-3 -> hue)
// keccak256('') = c5d24601..., keccak256('abc') = 4e03657a...ea45...: hue = bytes 2-3 mod 360 (0x4601 -> 281, 0x657a -> 58)
const f = (a) => identicon(a).cells.map((r) => r.map((x) => (x ? 1 : 0)).join('')).join('/');
assert.equal(identicon('').hue, 281);
assert.equal(f(''), '10101/00000/11011/10001/10101');
assert.equal(identicon('abc').hue, 58);
assert.equal(f('abc'), '01110/10001/10101/10001/00000');
assert.equal(groupedShort('SM64' + 'x'.repeat(91)), 'SM64 xxxx xxxx … xxxx xxxx');
console.log('identicon ok');
