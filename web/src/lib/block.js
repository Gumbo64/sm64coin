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


// ---- Keccak-256 (the original padding 0x01, as Monero's cn_fast_hash; NOT SHA3-256) ----

const RC = [
  0x00000000, 0x00000001, 0x00000000, 0x00008082, 0x80000000, 0x0000808a, 0x80000000, 0x80008000,
  0x00000000, 0x0000808b, 0x00000000, 0x80000001, 0x80000000, 0x80008081, 0x80000000, 0x00008009,
  0x00000000, 0x0000008a, 0x00000000, 0x00000088, 0x00000000, 0x80008009, 0x00000000, 0x8000000a,
  0x00000000, 0x8000808b, 0x80000000, 0x0000008b, 0x80000000, 0x00008089, 0x80000000, 0x00008003,
  0x80000000, 0x00008002, 0x80000000, 0x00000080, 0x00000000, 0x0000800a, 0x80000000, 0x8000000a,
  0x80000000, 0x80008081, 0x80000000, 0x00008080, 0x00000000, 0x80000001, 0x80000000, 0x80008008,
]; // [hi, lo] per round
const ROTC = [1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14, 27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44];
const PILN = [10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4, 15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1];

/** Keccak-f[1600] on 25 lanes held as s[2*i] = lo, s[2*i+1] = hi (Uint32Array(50)). */
function keccakF(s) {
  const bc = new Uint32Array(10);
  for (let round = 0; round < 24; round++) {
    for (let x = 0; x < 5; x++) {
      bc[2 * x] = s[2 * x] ^ s[2 * (x + 5)] ^ s[2 * (x + 10)] ^ s[2 * (x + 15)] ^ s[2 * (x + 20)];
      bc[2 * x + 1] = s[2 * x + 1] ^ s[2 * (x + 5) + 1] ^ s[2 * (x + 10) + 1] ^ s[2 * (x + 15) + 1] ^ s[2 * (x + 20) + 1];
    }
    for (let x = 0; x < 5; x++) {
      const a = (x + 4) % 5, b = (x + 1) % 5;
      const lo = bc[2 * b], hi = bc[2 * b + 1];
      const tlo = bc[2 * a] ^ ((lo << 1) | (hi >>> 31));
      const thi = bc[2 * a + 1] ^ ((hi << 1) | (lo >>> 31));
      for (let y = 0; y < 25; y += 5) { s[2 * (y + x)] ^= tlo; s[2 * (y + x) + 1] ^= thi; }
    }
    // rho and pi
    let tlo = s[2], thi = s[3];
    for (let i = 0; i < 24; i++) {
      const j = PILN[i], r = ROTC[i];
      const nlo = s[2 * j], nhi = s[2 * j + 1];
      let rlo, rhi;
      if (r < 32) { rlo = (tlo << r) | (thi >>> (32 - r)); rhi = (thi << r) | (tlo >>> (32 - r)); }
      else if (r === 32) { rlo = thi; rhi = tlo; }
      else { const q = r - 32; rlo = (thi << q) | (tlo >>> (32 - q)); rhi = (tlo << q) | (thi >>> (32 - q)); }
      s[2 * j] = rlo; s[2 * j + 1] = rhi;
      tlo = nlo; thi = nhi;
    }
    // chi
    for (let y = 0; y < 25; y += 5) {
      for (let x = 0; x < 5; x++) { bc[2 * x] = s[2 * (y + x)]; bc[2 * x + 1] = s[2 * (y + x) + 1]; }
      for (let x = 0; x < 5; x++) {
        const b1 = (x + 1) % 5, b2 = (x + 2) % 5;
        s[2 * (y + x)] ^= ~bc[2 * b1] & bc[2 * b2];
        s[2 * (y + x) + 1] ^= ~bc[2 * b1 + 1] & bc[2 * b2 + 1];
      }
    }
    // iota
    s[1] ^= RC[2 * round];
    s[0] ^= RC[2 * round + 1];
  }
}

/** Monero's `cn_fast_hash`: Keccak-256 of a Uint8Array -> Uint8Array(32). */
export function keccak256(data) {
  const rate = 136;
  const padded = new Uint8Array(Math.ceil((data.length + 1) / rate) * rate);
  padded.set(data);
  padded[data.length] ^= 0x01;
  padded[padded.length - 1] ^= 0x80;
  const s = new Uint32Array(50);
  const dv = new DataView(padded.buffer);
  for (let off = 0; off < padded.length; off += rate) {
    for (let i = 0; i < rate / 4; i++) s[i] ^= dv.getUint32(off + 4 * i, true);
    keccakF(s);
  }
  const out = new Uint8Array(32);
  const ov = new DataView(out.buffer);
  for (let i = 0; i < 8; i++) ov.setUint32(4 * i, s[i], true);
  return out;
}

// ---- The committed proof blob (v0.4, docs/V0.4-DESIGN.md 1.2; pow/sm64pow/src/commit.rs) ----
//   varint(frames) || start (0|1) || input_hash (32) || codec id || body
//   input_hash = keccak256("sm64run2" || frames u32 LE || start || 4 bytes per non-poked input pad)

export const MAX_FRAMES = 1 << 20;
const ascii = (s) => Uint8Array.from(s, (c) => c.charCodeAt(0));
const cat = (...parts) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let o = 0;
  for (const p of parts) { out.set(p, o); o += p.length; }
  return out;
};

/** The committed hash of a run: `pads` the non-poked frames' inputs as raw bytes (4 per pad: buttons u16 LE, x i8, y i8). */
export function inputHash(frames, start, pads) {
  const head = new Uint8Array(5);
  new DataView(head.buffer).setUint32(0, frames, true);
  head[4] = start;
  return keccak256(cat(ascii('sm64run2'), head, pads));
}

/** [frames, start, hash(32), encodingOffset] of a committed blob, or null when its header does not parse
 * (truncated, non-canonical or over-10-byte varint, frames > 1<<20, start > 1, fewer than 32 hash bytes). */
export function parseHeader(blob) {
  let v = 0n, i = 0, done = false;
  for (let shift = 0n; shift < 64n; shift += 7n) {
    if (i >= blob.length) return null;
    const b = blob[i++];
    v |= BigInt(b & 0x7f) << shift;
    if (!(b & 0x80)) {
      if (b === 0 && shift > 0n) return null;
      done = true;
      break;
    }
  }
  if (!done) return null;
  v &= (1n << 64n) - 1n;
  if (v > BigInt(MAX_FRAMES)) return null;
  if (i >= blob.length || blob[i] > 1) return null;
  if (blob.length < i + 33) return null;
  return { frames: Number(v), start: blob[i], hash: blob.slice(i + 1, i + 33), encoding: i + 33 };
}

/** What a block id and the signature commit to: the header's input hash (no decoding); a blob whose header
 * does not parse gets keccak256("sm64bad!" || blob). */
export function proofId(blob) {
  const h = parseHeader(blob);
  return h ? h.hash : keccak256(cat(ascii('sm64bad!'), blob));
}

/** A committed blob with codec 0 (raw): `pads` the non-poked frames' inputs as raw bytes. */
export function committedProof(frames, start, pads) {
  return cat(Uint8Array.from(varint(frames)), Uint8Array.of(start), inputHash(frames, start, pads), Uint8Array.of(0), pads);
}
