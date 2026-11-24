// Web Worker: replays blocks in the headless game (the build native nodes run) with the nodes'
// own pokes and proof codecs (perturb-wasm): see core.js. The game stays at the end of the last
// block it played, so the next request (usually: one new block on top) continues from there.
//
// Message in: {id, items: [{hash, height, start, frames, pads?: ArrayBuffer, blk?, proof?}]}, a
// cycle's blocks in order (from its first block). Items with `pads` were replayed before (their
// played pads are re-played unperturbed, which reaches the same state); the others are replayed
// from their proof. Out: {id, type: 'block', hash, pads, poked, frames, canonical, event,
// progress} per replayed block, {id, type: 'progress', done, total}, then {id, type: 'done'} or
// {id, type: 'error', message}.
import { Sm64HeadlessEngine } from 'sm64-binds-src/scripts/sm64/Sm64Headless.js';
import { getHeadlessWASM } from 'sm64-binds-src/scripts/headlessWasm.js';
import init, * as wasm from '../../wasm/perturb/sm64coin_perturb_wasm.js';
import * as core from './core.js';

let ready = null;
let engine = null;
let progressBytes = null;
let atHash = null; // the block whose end state the game is at (null: unknown)

async function setup() {
  await init();
  engine = await Sm64HeadlessEngine.create(await getHeadlessWASM());
  const flat = wasm.headless_progress_regions();
  const regions = [];
  let len = 0;
  for (let i = 0; i < flat.length; i += 2) { regions.push([flat[i], flat[i + 1]]); len += flat[i + 1]; }
  progressBytes = () => {
    const out = new Uint8Array(len);
    let o = 0;
    for (const [a, l] of regions) { out.set(new Uint8Array(engine.memory.buffer, a, l), o); o += l; }
    return out;
  };
}

let queue = Promise.resolve();
self.onmessage = (e) => { queue = queue.then(() => run(e.data)); };

async function run({ id, items }) {
  try {
    ready ??= setup();
    await ready;
    if (!items.length) return; // a warm-up: the game is ready, its position is kept
    // Resume after the last block of this cycle the game is already at.
    let from = 0;
    const k = items.findIndex((it) => it.hash === atHash);
    if (k >= 0) from = k + 1;
    else atHash = null;
    const total = items.slice(from).reduce((s, it) => s + it.frames, 0);
    let done = 0;
    for (let i = from; i < items.length; i++) {
      const it = items[i];
      atHash = null; // mid-block until this one completes
      if (it.start === 1 || it.height === 1 || i === 0) engine.restart();
      if (it.pads) {
        core.playPads(engine, new Uint8Array(it.pads));
      } else {
        const before = progressBytes();
        const r = core.replayBlock(engine, wasm, it.blk, it.proof, (f, n) => {
          if ((f & 4095) === 4095) self.postMessage({ id, type: 'progress', done: done + f, total });
        });
        const after = progressBytes();
        const p = Array.from(wasm.progress_points(after));
        const b = Array.from(wasm.progress_points(before));
        self.postMessage({ id, type: 'block', hash: it.hash, pads: r.pads.buffer, poked: r.poked.buffer, frames: r.frames, canonical: r.canonical,
          event: wasm.progress_event(before, after), progress: p, gained: p[3] - b[3] }, [r.pads.buffer, r.poked.buffer]);
      }
      done += it.frames;
      atHash = it.hash;
    }
    self.postMessage({ id, type: 'done' });
  } catch (err) {
    atHash = null;
    self.postMessage({ id, type: 'error', message: String(err?.message || err) });
  }
}
