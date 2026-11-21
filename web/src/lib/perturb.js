// The controller perturbation in the browser: the same Rust code native
// nodes run (pow/perturb, compiled to WASM in src/wasm/perturb). It lives
// outside the game: before and after each frame we copy the adapter's memory
// regions (the game clock and a few state fields, at the visual build's
// addresses) out of the game and hand them over.
import init, { WasmPerturber, visual_regions } from '../wasm/perturb/sm64coin_perturb_wasm.js';
import { GamePad } from 'sm64-binds-frontend';

let ready = null;
export const perturbReady = () => (ready ??= init());

let REGIONS = null;
function readRegions(engine) {
  if (!REGIONS) {
    const r = visual_regions();
    REGIONS = [];
    for (let i = 0; i < r.length; i += 2) REGIONS.push([r[i], r[i + 1]]);
  }
  const total = REGIONS.reduce((s, [, l]) => s + l, 0);
  const out = new Uint8Array(total);
  const mem = engine.memory.buffer; // re-read: growth replaces the buffer
  let o = 0;
  for (const [a, l] of REGIONS) {
    out.set(new Uint8Array(mem, a, l), o);
    o += l;
  }
  return out;
}

/** A block's perturbation, for one game from power-on. */
export class Perturbation {
  constructor(task) {
    const c = task.config.random_config;
    this.p = new WasmPerturber(task.sm64_seed_hash, c.window_length, c.random_amount, c.random_burst_length, c.A_prob, c.B_prob, c.Z_prob);
  }

  /** Plays one frame: returns [the pad played, whether it was poked]. */
  step(engine, wanted) {
    const before = readRegions(engine);
    const r = this.p.pad(before, wanted.button, wanted.stick_x, wanted.stick_y);
    const played = new GamePad(r[0], r[1], r[2]);
    engine.step_game(played);
    this.p.after(before, readRegions(engine));
    return [played, r[3] === 1];
  }

  free() {
    this.p.free();
  }
}
