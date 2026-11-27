import assert from 'node:assert/strict';
import { makeBook, webApi, checkLabel, normalize, labelOf, findByAddress, looksLikeAddress, MAX_LABEL, shortAddress } from './addrbook.js';

const A = 'SM64RtPqsHM63LZn2u9Myx9HtC4kjhu1h98SRtJz9USB7NZAoCMdZSBD7D1L2DWu9uCYZFUSVKytreuzEMJ6VjBU2tbjKqevFH';
const B = 'SM6' + 'b'.repeat(95);
const C = 'SM6' + 'c'.repeat(95);

// a mocked monero-ts wallet (the shape of MoneroWalletFull's address book methods; entries are MoneroAddressBookEntry-like)
function mockWallet() {
  const rows = []; let next = 0; const calls = [];
  const entry = (r) => ({ getIndex: () => r.index, getAddress: () => r.address, getDescription: () => r.description });
  return {
    rows, calls,
    async getAddressBookEntries() { calls.push('get'); return rows.map(entry); },
    async addAddressBookEntry(address, description) { calls.push('add'); rows.push({ index: next, address, description }); return next++; },
    async editAddressBookEntry(index, setAddress, address, setDescription, description) {
      calls.push(['edit', setAddress, setDescription]);
      const r = rows.find((x) => x.index === index); if (!r) throw new Error('bad index');
      if (setAddress) r.address = address; if (setDescription) r.description = description;
    },
    async deleteAddressBookEntry(index) { calls.push('del'); const i = rows.findIndex((x) => x.index === index); if (i < 0) throw new Error('bad index'); rows.splice(i, 1); },
  };
}
const validate = (a) => { if (!/^SM6[a-zA-Z0-9]{90,110}$/.test(a) || a === 'SM6' + 'x'.repeat(95)) throw new Error('invalid'); };

// pure helpers
assert.ok(looksLikeAddress(A) && !looksLikeAddress('4AdU...') && !looksLikeAddress('') && !looksLikeAddress('SM6short'));
assert.deepEqual(checkLabel('  Alice '), { ok: true, value: 'Alice', error: '' });
assert.equal(checkLabel('x'.repeat(MAX_LABEL + 1)).ok, false);
assert.equal(checkLabel('a\nb').ok, false);
assert.equal(checkLabel('Alice\u202eecilA').ok, false, 'a bidi override');
assert.equal(checkLabel('Zoë 💰 東京').ok, true);
assert.equal(checkLabel(undefined).ok, true);
assert.equal(shortAddress(A), `${A.slice(0, 8)}…${A.slice(-6)}`);
assert.deepEqual(normalize([{ index: 2, address: 'z', description: 'bob' }, { index: 0, address: 'y', description: '' }, { index: 1, address: 'x', description: 'Alice' }]).map((e) => e.index), [1, 2, 0], 'by label, unlabelled last');

// CRUD through the mocked monero-ts
let saved = 0;
const w = mockWallet();
const book = makeBook(webApi(w, validate, async () => { saved++; }));
assert.deepEqual(await book.list(), []);
const i1 = await book.add(` ${A} `, '  Alice  ');
assert.equal(i1, 0);
const i2 = await book.add(B, 'bob');
assert.equal(w.rows[0].address, A); assert.equal(w.rows[0].description, 'Alice', 'trimmed');
let l = await book.list();
assert.deepEqual(l.map((e) => [e.index, e.label]), [[0, 'Alice'], [1, 'bob']]);
assert.equal(labelOf(l, B), 'bob'); assert.equal(labelOf(l, C), ''); assert.equal(findByAddress(l, A).index, 0);
assert.equal(saved, 2, 'the wallet is saved after each write');

// rules: duplicates, bad addresses, bad labels
await assert.rejects(book.add(A, 'again'), /Already in the address book as "Alice"/);
await assert.rejects(book.add('4AdUndXHHZ6cfufTMvppY6JwXNouMBzSkbLYfpAV5Usx3skxNgYeYTRj5UzqtReoS44qo9mtmXCqY45DJ852K5Jv2684Rge', 'monero'), /not an SM64Coin address/);
await assert.rejects(book.add('SM6' + 'x'.repeat(95), 'bad checksum'), /not a valid SM64Coin address/);
await assert.rejects(book.add(C, 'x'.repeat(200)), /up to 120/);
assert.equal(w.rows.length, 2, 'nothing was saved for a refused entry');

// update: label only (the address is not sent), address only, both
await book.edit(i1, { label: 'Alice B.' });
assert.deepEqual(w.calls.at(-1), ['edit', false, true]);
assert.equal(w.rows[0].description, 'Alice B.'); assert.equal(w.rows[0].address, A);
await book.edit(i1, { address: C });
assert.deepEqual(w.calls.at(-1), ['edit', true, false]);
assert.equal(w.rows[0].address, C);
await assert.rejects(book.edit(i1, { address: B }), /Already in the address book as "bob"/);
await book.edit(i1, { label: 'Alice C.' }); // an entry may keep its own address
await assert.rejects(book.edit(99, { label: 'x' }), /gone/);
await assert.rejects(book.edit(i2, { address: 'SM6short' }), /not an SM64Coin address/);

// delete
await book.remove(i2);
l = await book.list();
assert.deepEqual(l.map((e) => e.index), [0]);
await book.add(B, ''); // an unlabelled entry
assert.equal((await book.list()).at(-1).label, '');
console.log('addrbook ok');
