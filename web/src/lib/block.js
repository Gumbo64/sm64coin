// Block-blob helpers without browser dependencies (node tests import this; proof.js re-exports them).

export function hexToBytes(hex) {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.substr(i * 2, 2), 16);
  return out;
}

export function bytesToHex(bytes) {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

export function varint(n) {
  const out = [];
  while (n >= 0x80) { out.push((n & 0x7f) | 0x80); n = Math.floor(n / 128); }
  out.push(n);
  return out;
}

/** The v0.3 block: the template (ending with the empty proof, a 0x00 byte, and the empty signature, 64 zero
 * bytes) with varint(len(proof)) || proof || sig in their place. `sig`: 64 bytes (or hex). */
export function seal(templateHex, proof, sig) {
  const t = hexToBytes(templateHex);
  if (typeof sig === 'string') sig = hexToBytes(sig);
  if (!sig || sig.length !== 64) throw new Error('a block needs a 64-byte signature');
  const n0 = t.length;
  if (n0 < 65 || t.subarray(n0 - 65).some((b) => b !== 0)) throw new Error('template does not end with an empty proof and signature; is this an SM64Coin v0.3 daemon?');
  const len = varint(proof.length);
  const out = new Uint8Array(n0 - 65 + len.length + proof.length + 64);
  out.set(t.subarray(0, n0 - 65), 0);
  out.set(len, n0 - 65);
  out.set(proof, n0 - 65 + len.length);
  out.set(sig, n0 - 65 + len.length + proof.length);
  return bytesToHex(out);
}

