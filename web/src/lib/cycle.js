// The cycle: where a block's run starts. Each block continues from its
// parent's end state, so to play one the browser first brings the visual
// engine to that state: it replays the cycle's played pads (the daemon's
// get_sm64_cycle) unperturbed from power-on, as fast as it can, and caches
// the result as a save state (lib/savestate.js) so the next block of the same
// cycle only replays its own pads.
import { Sm64VisualEngine, GamePad } from 'sm64-binds-frontend';
import { perturbReady, readProgress } from './perturb.js';
import { captureState, restoreState } from './savestate.js';
import { saveTip, loadTips } from './tipstore.js';
import { hexToBytes } from './proof.js';
import { daemon } from './rpc.js';

/** A game, powered on, rendering to `canvas`. One engine serves a whole page:
 * it is reset with save states, never recreated (the GL objects the game's
 * memory refers to belong to the engine). */
export async function createEngine(canvas) {
  await perturbReady();
  canvas.id = 'canvas'; // the emscripten build looks the canvas up by id
  const engine = await Sm64VisualEngine.create(canvas, NaN, null);
  canvas.id = '';
  engine.canvas = canvas;
  engine.powerOn = captureState(engine); // frame 0: what restart means
  engine.cycleCache = []; // [{hash, frames, state}], newest last
  // The tips saved by earlier visits (lib/tipstore.js): a reload needs no replay of the cycle.
  engine.cycleCache.push(...(await loadTips(engine)));
  return engine;
}

// Skips the GL draw calls while fast-forwarding (state and textures still
// update; only the pixels aren't drawn).
export function muteDrawing(engine) {
  let gl = null;
  try { gl = engine.canvas.getContext('webgl2') || engine.canvas.getContext('webgl'); } catch { /* no context */ }
  if (!gl) return () => {};
  const saved = {};
  for (const k of ['drawElements', 'drawArrays', 'clear']) {
    saved[k] = gl[k];
    gl[k] = () => {};
  }
  return () => { for (const k in saved) gl[k] = saved[k]; };
}

const yieldUi = () => new Promise((r) => setTimeout(r, 0));

/**
 * Plays raw pads (4 bytes per frame: buttons u16 LE, stick x, stick y)
 * `from..to` through the engine unperturbed, as fast as it goes, yielding to
 * the page now and then. `onProgress(done, total)`; resolves false if
 * `kill()` says stop.
 */
export async function fastForward(engine, raw, from, to, onProgress = () => {}, kill = () => false) {
  const pad = new GamePad(0, 0, 0);
  engine.set_audio_enabled(0);
  const unmute = muteDrawing(engine);
  try {
    let i = from;
    while (i < to) {
      const t0 = performance.now();
      for (let n = 1; i < to; n++) {
        const o = i * 4;
        pad.button = raw[o] | (raw[o + 1] << 8);
        pad.stick_x = (raw[o + 2] << 24) >> 24;
        pad.stick_y = (raw[o + 3] << 24) >> 24;
        engine.step_game(pad);
        i++;
        if ((n & 63) === 0 && performance.now() - t0 > 30) break;
      }
      onProgress(i - from, to - from);
      if (kill()) return false;
      await yieldUi();
    }
    return true;
  } finally {
    unmute();
  }
}

const CACHE_TIPS = 3;

/**
 * The engine at the end of the cycle that ends at block `hash`, with its
 * state: restores the nearest cached tip of the same cycle (or power-on) and
 * fast-forwards the rest. `cycle` is get_sm64_cycle's answer for `hash`.
 * Returns {state, progress, frames, replayed} (state: a save state of the
 * engine there), or null if `kill()` stopped it.
 */
export async function reachTip(engine, cycle, onProgress = () => {}, kill = () => false) {
  const raw = hexToBytes(cycle.played_pads);
  const total = raw.length / 4;
  const hit = engine.cycleCache.find((c) => c.hash === cycle.hash);
  if (hit) {
    restoreState(engine, null, hit.state);
    return { state: hit.state, progress: readProgress(engine), frames: total, replayed: 0 };
  }
  // The latest cached block of this cycle: frames before it are already played.
  let base = { state: engine.powerOn, frames: 0 };
  let at = 0;
  cycle.blocks.forEach((b, i) => {
    at += b.frames;
    const c = engine.cycleCache.find((x) => x.hash === b.hash && x.frames === at);
    if (c) base = { state: c.state, frames: at };
  });
  restoreState(engine, null, base.state);
  const ok = await fastForward(engine, raw, base.frames, total, (d, t) => onProgress(base.frames + d, total), kill);
  if (!ok) return null;
  const state = captureState(engine, null, engine.powerOn);
  engine.cycleCache.push({ hash: cycle.hash, frames: total, state });
  saveTip(engine, { hash: cycle.hash, frames: total, state }); // kept across reloads (not awaited)
  while (engine.cycleCache.length > CACHE_TIPS) engine.cycleCache.shift();
  return { state, progress: readProgress(engine), frames: total, replayed: total - base.frames };
}

/** Same stars/keys as the daemon says the tip has? */
export function matchesTip(progress, cycle) {
  return progress.stars === cycle.stars && progress.keys === cycle.keys && progress.grand === cycle.grand_star;
}

/**
 * The start a block template asks for: {kind: 'power-on'|'parent', state,
 * progress, cycle}. `forcePowerOn` is for a stalled chain (sm64_start
 * "either"). Resolves null if `kill()` stopped the fast-forward.
 */
export async function prepareStart(engine, tpl, forcePowerOn, onProgress, kill) {
  if (forcePowerOn || tpl.sm64_start === 'power-on') {
    restoreState(engine, null, engine.powerOn);
    return { kind: 'power-on', state: engine.powerOn, progress: readProgress(engine), cycle: null };
  }
  const cycle = await daemon('get_sm64_cycle', { hash: tpl.prev_hash });
  const r = await reachTip(engine, cycle, onProgress, kill);
  if (!r) return null;
  if (!matchesTip(r.progress, cycle)) {
    throw new Error(`replaying the cycle reached ${r.progress.stars} stars and ${r.progress.keys} keys, the node says ${cycle.stars} and ${cycle.keys}`);
  }
  return { kind: 'parent', state: r.state, progress: r.progress, cycle };
}
