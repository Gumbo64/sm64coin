// The committed proof blob (v0.4): keccak256, inputHash, proofId and committedProof against the Rust vectors.
// Run: node src/lib/proofid.test.mjs
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { keccak256, inputHash, proofId, committedProof, parseHeader, hexToBytes, bytesToHex } from './block.js';

const enc = (s) => new TextEncoder().encode(s);
assert.equal(bytesToHex(keccak256(new Uint8Array(0))), 'c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470');
assert.equal(bytesToHex(keccak256(enc('abc'))), '4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45');
assert.notEqual(bytesToHex(keccak256(new Uint8Array(135))), bytesToHex(keccak256(new Uint8Array(136)))); // the rate boundary

const here = new URL('.', import.meta.url);
const lines = (p) => readFileSync(new URL(p, here), 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l));

const vectors = lines('../../wallet-wasm/test/vectors-v04.jsonl');
const blobs = new Map(vectors.map((v) => [v.name, v.blob]));
let ids = 0, runs = 0, rebuilt = 0;
for (const v of vectors) {
  const blob = hexToBytes(v.blob);
  assert.equal(bytesToHex(proofId(blob)), v.id, v.name);
  ids++;
  const h = parseHeader(blob);
  assert.equal(h !== null, v.header, v.name);
  if (h) assert.deepEqual([h.frames, h.start], [v.frames, v.start], v.name);
  if (v.kind === 'run') {
    const pads = hexToBytes(v.pads);
    assert.equal(bytesToHex(inputHash(v.frames, v.start, pads)), v.input_hash, v.name);
    runs++;
    if (v.name.endsWith('/raw')) {
      assert.equal(bytesToHex(committedProof(v.frames, v.start, pads)), v.blob, v.name);
      rebuilt++;
    }
  }
}
assert.ok(ids > 200 && runs > 0 && rebuilt > 0);

const sigs = lines('../../../python/tests/sig_vectors_web_v04.jsonl');
for (const s of sigs) assert.equal(bytesToHex(proofId(hexToBytes(s.proof))), s.proof_id);

console.log(`proofid.test.mjs: ${ids} ids, ${runs} run hashes, ${rebuilt} rebuilt raw blobs, ${sigs.length} signature proof ids ok`);
