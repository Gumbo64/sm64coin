// Hybrid mining without asking the search: the page rebuilds a node's exact state itself. The worker sends, once per
// step, the step's start state (the headless game's SaveState bytes) and the root's perturbation + sync tag; the page's
// tree mirror gives the node's path (the wanted pads from the root). Load the root, replay the path through the block's
// perturbation muted at full speed, and the game and the perturbation are exactly what the search had at the node.
import { GamePad } from 'sm64-binds-frontend';
import { encPads } from '../../lib/webminer/hybrid.js';

/** What `buildLocal` needs for node `sel` ({w, id, step}), or null when the page does not have it (use the mailbox). */
export function localInputs(hub, sel) {
  const w = hub.workers[sel.w];
  if (!w || !w.cur || !hub.isCurrent(sel.slot)) return null;
  const key = w.cur.key;
  if (!w.root || w.root.key !== key || !w.rootPert || w.rootPert.key !== key) return null;
  const raw = hub.nodePath(sel.w, key, sel.id);
  if (!raw) return null;
  return { key, rootRaw: w.root.raw, pert: w.rootPert.pert, tag: w.rootPert.tag, raw, sk: `${sel.w}:${key}` };
}

const yieldUi = () => new Promise((r) => setTimeout(r, 0));

/**
 * Puts `engine` at the node and returns {start (a Recorder start), msg (what the mailbox's state answer would carry)}.
 * The caller holds the engine's lock. `kill()` -> true aborts (resolves null).
 */
export async function buildLocal(engine, hub, sel, inp, onProgress = () => {}, kill = () => false) {
  const [{ Perturbation, perturbReady, readProgress }, { withConfig }, { restoreState, captureState }, { portableFor }, { muteDrawing }] = await Promise.all([
    import('../../lib/perturb.js'), import('../../lib/task.js'), import('../../lib/savestate.js'), import('./startState.js'), import('../../lib/cycle.js'),
  ]);
  await perturbReady();
  const portable = await portableFor(hub, inp.sk, inp.rootRaw);
  if (kill()) return null;
  restoreState(engine, null, engine.powerOn);
  engine.load_portable_state(portable);
  const task = withConfig(hub.template);
  const pert = new Perturbation(task);
  const pads = [];
  const total = inp.raw.length / 4;
  const unmute = muteDrawing(engine);
  engine.set_audio_enabled(0);
  try {
    pert.loadBytes(new TextEncoder().encode(inp.pert));
    let t0 = performance.now();
    for (let i = 0; i < total; i++) {
      const b = inp.raw[i * 4] | (inp.raw[i * 4 + 1] << 8), x = (inp.raw[i * 4 + 2] << 24) >> 24, y = (inp.raw[i * 4 + 3] << 24) >> 24;
      pert.step(engine, new GamePad(b, x, y));
      pads.push({ button: b, stick_x: x, stick_y: y });
      if (performance.now() - t0 > 40) { onProgress(i, total); if (kill()) return null; await yieldUi(); t0 = performance.now(); }
    }
  } finally { unmute(); }
  const pertBytes = pert.toBytes();
  const start = { kind: 'node', state: captureState(engine, null, engine.powerOn), progress: readProgress(engine), pertBytes };
  pert.free();
  const msg = { level: sel.level, area: sel.area, stars: sel.stars, pos: sel.pos, tag: inp.tag, path: encPads(pads), pert: new TextDecoder().decode(pertBytes) };
  return { start, msg, task };
}
