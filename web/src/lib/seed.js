// The perturbation seed, mirroring pow/sm64pow/src/lib.rs `Seed`: the block's
// 32-byte seed hash, fed to the game 32 bits at a time. Before frame
// k * reseed_frames (k >= 1) the RNG state is reset to word(k).
export class Seed {
  constructor(hex) {
    if (!/^[0-9a-f]{64}$/i.test(hex)) throw new Error('seed hash must be 64 hex characters');
    this.bytes = Uint8Array.from({ length: 32 }, (_, i) => parseInt(hex.substr(i * 2, 2), 16));
  }
  word(k) {
    const i = (k % 8) * 4;
    const b = this.bytes;
    let w = (b[i] | (b[i + 1] << 8) | (b[i + 2] << 16) | (b[i + 3] << 24)) >>> 0;
    w = (w ^ Math.imul(Math.floor(k / 8), 0x9e3779b9)) >>> 0;
    return w === 0 ? 1 : w;
  }
  /** Call before frame `frame`'s rng_pad. */
  reseed(engine, reseedFrames, frame) {
    if (reseedFrames && frame > 0 && frame % reseedFrames === 0) engine.game._set_rng_seed(this.word(frame / reseedFrames));
  }
}
