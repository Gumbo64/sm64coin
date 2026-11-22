// The controller perturbation and the game's progress in the browser: the same
// Rust code native nodes run (pow/perturb, compiled to WASM in src/wasm/perturb).
// It lives outside the game: before and after each frame we copy the adapter's
// memory regions (the game clock and a few state fields, at the visual build's
// addresses) out of the game and hand them over.
import init, { WasmPerturber, visual_regions, visual_progress_regions, progress_points } from '../wasm/perturb/sm64coin_perturb_wasm.js';
import { GamePad } from 'sm64-binds-frontend';

let ready = null;
export const perturbReady = () => (ready ??= init());

const pairs = (r) => { const out = []; for (let i = 0; i < r.length; i += 2) out.push([r[i], r[i + 1]]); return out; };

let REGIONS = null;
let PROGRESS_REGIONS = null;
let BUF = null;
let PBUF = null;

function gather(engine, regions, out) {
  const mem = engine.memory.buffer; // re-read: growth replaces the buffer
  let o = 0;
  for (const [a, l] of regions) {
    out.set(new Uint8Array(mem, a, l), o);
    o += l;
  }
  return out;
}

function readRegions(engine) {
  if (!REGIONS) {
    REGIONS = pairs(visual_regions());
    BUF = new Uint8Array(REGIONS.reduce((s, [, l]) => s + l, 0));
  }
  return gather(engine, REGIONS, BUF);
}

/**
 * How far into the game the engine is: {stars, keys, grand, points}. One point
 * per star, per Bowser key and for the Grand Star (with 70+ stars): computed
 * by the same Rust code nodes judge blocks with. Needs perturbReady().
 */
export function readProgress(engine) {
  if (!PROGRESS_REGIONS) {
    PROGRESS_REGIONS = pairs(visual_progress_regions());
    PBUF = new Uint8Array(PROGRESS_REGIONS.reduce((s, [, l]) => s + l, 0));
  }
  const p = progress_points(gather(engine, PROGRESS_REGIONS, PBUF));
  return { stars: p[0], keys: p[1], grand: p[2] === 1, points: p[3] };
}

/** A block's perturbation, for one game from the state the run starts at. */
export class Perturbation {
  constructor(task) {
    const c = task.config.random_config;
    this.p = new WasmPerturber(task.sm64_seed_hash, c.window_length, c.random_amount, c.random_burst_length, c.A_prob, c.B_prob, c.Z_prob);
  }

  /** Plays one frame: returns [the pad played, whether it was poked]. */
  step(engine, wanted) {
    const before = readRegions(engine).slice();
    const r = this.p.pad(before, wanted.button, wanted.stick_x, wanted.stick_y);
    const played = new GamePad(r[0], r[1], r[2]);
    engine.step_game(played);
    this.p.after(before, readRegions(engine));
    return [played, r[3] === 1];
  }

  /** The perturbation's state, for save states. */
  toBytes() {
    return this.p.to_bytes();
  }

  /** Becomes the perturbation saved by toBytes() (same seed and config). */
  loadBytes(bytes) {
    this.p.load_bytes(bytes);
  }

  free() {
    this.p.free();
  }
}
