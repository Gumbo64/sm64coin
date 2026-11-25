import assert from 'node:assert/strict';
import { seal, bytesToHex, hexToBytes } from './block.js';
import { setSignerImpl, newMinerKey, templateParams, signRun } from './minerkey.js';

// seal: a v0.4 template ends with the empty proof (0x00), 64 zero bytes (the empty signature) and the empty proof records (0x00).
{
  const head = '0b0b8080e0c4a4a006' + '11'.repeat(40);
  const tpl = head + '00' + '00'.repeat(64) + '00';
  const proof = new Uint8Array([1, 2, 3]);
  const sig = new Uint8Array(64).map((_, i) => i + 1);
  assert.equal(seal(tpl, proof, sig), head + '03' + '010203' + bytesToHex(sig) + '00');
  assert.equal(seal(tpl, proof, bytesToHex(sig)), head + '03' + '010203' + bytesToHex(sig) + '00'); // hex sig
  // A proof of 200 bytes: two-byte varint length.
  assert.equal(seal(tpl, new Uint8Array(200), sig).slice(head.length, head.length + 4), 'c801');
  assert.throws(() => seal(head + '00', proof, sig), /v0\.4/); // an old template (no signature field)
  assert.throws(() => seal(tpl.slice(0, -2) + '01', proof, sig), /v0\.4/);
  assert.throws(() => seal(tpl.slice(0, -2), proof, sig), /v0\.4/); // a v0.3 template (no records field)
  assert.throws(() => seal(tpl, proof, new Uint8Array(63)), /64-byte/);
}

// The key comes from 32 random bytes, and every template request (also the sm64_frames one) carries its extra nonce.
{
  const seen = [];
  setSignerImpl({
    miner_keypair: (e) => { seen.push(e); assert.equal(e.length, 32); return JSON.stringify({ secret_hex: 'aa'.repeat(32), public_hex: 'bb'.repeat(32), extra_nonce_hex: '64' + 'bb'.repeat(32) }); },
    sign_proof: (secret, seed, proof) => { assert.equal(secret, 'aa'.repeat(32)); assert.equal(seed, 'cc'.repeat(32)); assert.deepEqual([...proof], [9]); return 'dd'.repeat(64); },
  });
  const key = await newMinerKey();
  assert.equal(seen.length, 1);
  const p = templateParams('SM6addr', key);
  assert.deepEqual(p, { wallet_address: 'SM6addr', extra_nonce: '64' + 'bb'.repeat(32) });
  assert.ok(!('reserve_size' in p));
  const q = templateParams('SM6addr', key, { sm64_frames: 123 });
  assert.equal(q.extra_nonce, p.extra_nonce);
  assert.equal(q.sm64_frames, 123);
  assert.equal(await signRun(key, 'cc'.repeat(32), new Uint8Array([9])), 'dd'.repeat(64));
}
console.log('minerkey ok');
