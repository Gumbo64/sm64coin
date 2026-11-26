// Client-side replay of blocks: what a node does to verify a block, in the browser (or Node),
// so pages get the played pads (pokes included) without asking a node to replay anything.
//
// The game is the headless build (the one native nodes run, in a Web Worker here); the pokes
// and the state codecs are the nodes' own Rust code compiled to WASM (pow/perturb-wasm). Per
// frame, exactly like sm64pow's replay (SM64MiningGame::step / step_with):
//   1. the perturbation decides from the game clock whether this frame is poked;
//   2. a poked frame plays the poke; otherwise the proof's next input (stateless codecs) or the
//      state codec's decoded input (codecs 3+, decoded from the game state before the frame);
//   3. the game steps, and the perturbation settles (mixes the state digest in).
// Engine-agnostic: `engine` has `memory` (a WebAssembly.Memory), `step_game({button, stick_x,
// stick_y})` and `restart()`; `wasm` is the perturb-wasm module, initialised.

/** Hex string -> bytes. */
export function hexToBytes(hex) {
  const out = new Uint8Array(hex.length >> 1);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.substr(i * 2, 2), 16);
  return out;
}

/** A block's proof blob: the `sm64_proof_bytes` bytes before the 64-byte `sm64_sig`, which the
 * v0.4 proof-records field (`sm64_records_bytes`, at least its 1-byte empty length) follows at the end
 * of the serialized block (get_block's `blob`; docs/V0.4-DESIGN.md section 2.2). */
export const SIG_BYTES = 64;
export function blockProof(blk) {
  const bytes = hexToBytes(blk.blob);
  const end = bytes.length - (blk.sm64_records_bytes ?? 1) - SIG_BYTES;
  return bytes.slice(end - blk.sm64_proof_bytes, end);
}

/** `varint(frames) || start (0 parent, 1 power-on) || input_hash (32) || codec || body` (v0.4, pow/sm64pow/src/commit.rs). */
export function splitProof(p) {
  let frames = 0, shift = 0, i = 0;
  for (;;) {
    if (i >= p.length || shift > 35) throw new Error('bad proof: frame count');
    const b = p[i++];
    frames += (b & 0x7f) * 2 ** shift;
    shift += 7;
    if (!(b & 0x80)) break;
  }
  if (i + 34 > p.length) throw new Error('bad proof: header');
  return { frames, start: p[i], hash: p.subarray(i + 1, i + 33), codec: p[i + 33], body: p.subarray(i + 34) };
}

export const STATE_CODEC_MIN = 3;

/** Raw pads hex (4 bytes per frame) -> flat Int32Array [button, stick_x, stick_y, ...]. */
export function rawHexToFlat(hex) {
  const b = hexToBytes(hex);
  const out = new Int32Array((b.length >> 2) * 3);
  for (let i = 0, j = 0; i + 4 <= b.length; i += 4, j += 3) {
    out[j] = b[i] | (b[i + 1] << 8);
    out[j + 1] = (b[i + 2] << 24) >> 24;
    out[j + 2] = (b[i + 3] << 24) >> 24;
  }
  return out;
}

function regionReader(wasm, which) {
  const flat = which === 'progress' ? wasm.headless_progress_regions() : wasm.headless_regions();
  const regions = [];
  let len = 0;
  for (let i = 0; i < flat.length; i += 2) { regions.push([flat[i], flat[i + 1]]); len += flat[i + 1]; }
  return (engine) => {
    const out = new Uint8Array(len);
    const mem = engine.memory.buffer; // re-read: memory growth replaces the buffer
    let o = 0;
    for (const [a, l] of regions) { out.set(new Uint8Array(mem, a, l), o); o += l; }
    return out;
  };
}

/** [stars, keys, grand (0/1), points, caps]: the same code nodes judge progress with. */
export function makeProgress(wasm) {
  const read = regionReader(wasm, 'progress');
  return (engine) => Array.from(wasm.progress_points(read(engine)));
}

/**
 * Replays one block from the game as it is (the parent's end state; the caller restarts the
 * engine for a power-on start or a block on the genesis block). `blk` is get_block's answer
 * (without sm64_played). Returns {pads: Uint8Array (4 bytes per frame, like get_sm64_cycle's
 * played_pads), poked: Uint8Array (1 per frame), frames, canonical (state codecs: the proof is
 * the canonical encoding, as nodes require)}. `onFrame(i, frames)` may return true to stop
 * (then null is returned).
 */
export function replayBlock(engine, wasm, blk, proof, onFrame) {
  const read = regionReader(wasm, 'state');
  const { frames, codec, body } = proof;
  const pert = new wasm.WasmPerturber(blk.sm64_seed_hash, blk.sm64_window_length, blk.sm64_random_amount, blk.sm64_burst_length,
    blk.sm64_a_prob, blk.sm64_b_prob, blk.sm64_z_prob, blk.sm64_timing_mix ?? 1, blk.sm64_pad_mix ?? 1);
  const dec = codec >= STATE_CODEC_MIN ? new wasm.WasmStateDecoder(codec, body) : null;
  const inputs = dec ? null : rawHexToFlat(blk.sm64_pads || '');
  const pads = new Uint8Array(frames * 4);
  const poked = new Uint8Array(frames);
  const pad = { button: 0, stick_x: 0, stick_y: 0 };
  let next = 0;
  try {
    for (let f = 0; f < frames; f++) {
      const before = read(engine);
      let wb = 0, wx = 0, wy = 0;
      if (!dec) {
        if (next * 3 >= inputs.length) throw new Error(`frame ${f} needs an input but the proof has run out`);
        wb = inputs[next * 3]; wx = inputs[next * 3 + 1]; wy = inputs[next * 3 + 2];
      }
      const r = pert.pad(before, wb, wx, wy);
      if (r[3] === 1) {
        pad.button = r[0]; pad.stick_x = r[1]; pad.stick_y = r[2];
        poked[f] = 1;
        if (dec) dec.poked();
      } else if (dec) {
        const w = dec.pad(before);
        pad.button = w[0]; pad.stick_x = w[1]; pad.stick_y = w[2];
      } else {
        pad.button = r[0]; pad.stick_x = r[1]; pad.stick_y = r[2];
        next++;
      }
      engine.step_game(pad);
      pert.after(before, read(engine));
      const o = f * 4;
      pads[o] = pad.button & 0xff; pads[o + 1] = (pad.button >> 8) & 0xff; pads[o + 2] = pad.stick_x & 0xff; pads[o + 3] = pad.stick_y & 0xff;
      if (onFrame && onFrame(f, frames)) return null;
    }
    let canonical = true;
    if (dec) {
      const re = dec.finish();
      canonical = re.length === body.length && re.every((x, i) => x === body[i]);
    } else {
      canonical = next * 3 === inputs.length;
    }
    return { pads, poked, frames, canonical };
  } finally {
    pert.free();
    if (dec) dec.free();
  }
}

/** Plays raw pads (4 bytes per frame) unperturbed: reaches the same state the run did. */
export function playPads(engine, pads, from = 0, to = pads.length >> 2) {
  const pad = { button: 0, stick_x: 0, stick_y: 0 };
  for (let i = from; i < to; i++) {
    const o = i * 4;
    pad.button = pads[o] | (pads[o + 1] << 8);
    pad.stick_x = (pads[o + 2] << 24) >> 24;
    pad.stick_y = (pads[o + 3] << 24) >> 24;
    engine.step_game(pad);
  }
}

/** The event a block's run added (the same names nodes report: "cNNsK", "keyN", "grand"). */
export function eventName(before, after) {
  // [stars, keys, grand, points]; which star isn't in these counts: callers that need the
  // course/star use the node's event or the save file. A key or the Grand Star is exact.
  if (after[2] && !before[2]) return 'grand';
  if (after[1] > before[1]) return `key${after[1]}`;
  if (after[0] > before[0]) return null; // a star (with or without a cap in the same run): the star is the event, as in Rust and Python
  const caps = (after[4] || 0) & ~(before[4] || 0); // v0.5: wing 1, metal 2, vanish 4
  if (caps) return ['wing', 'metal', 'vanish'][31 - Math.clz32(caps & -caps)];
  return null;
}
