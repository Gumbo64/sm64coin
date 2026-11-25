// The reel's replays: a node's path is the search's *wanted* pads from its step's root, played there through the block's
// perturbation (a poke now and then replaces the pad), so a replay must play them through the same perturbation, loaded
// from the step's root as the worker sent it. Played plain (`step_game`) the pokes are missing and the game drifts away from
// where the search had the node. Every replay is checked against the position the search recorded for it.
import { GamePad } from 'sm64-binds-frontend';
import { muteDrawing } from '../../lib/cycle.js';
export { POS_TOL, checkPos } from './replayCheck.js';

export const padAt = (raw, i) => new GamePad(raw[i * 4] | (raw[i * 4 + 1] << 8), (raw[i * 4 + 2] << 24) >> 24, (raw[i * 4 + 3] << 24) >> 24);

/** A Perturbation armed like the step's root (`rootPert`: {pert: JSON text}), or the block's fresh one when `rootPert` is null. */
export function makePert(Perturbation, task, rootPert) {
  const p = new Perturbation(task);
  if (rootPert) p.loadBytes(new TextEncoder().encode(rootPert.pert));
  return p;
}

const yieldUi = () => new Promise((r) => setTimeout(r, 0));

/** Plays raw pads `from..to` through `pert`, muted and as fast as it goes (yielding to the page); false if `kill()` stopped it. */
export async function fastForwardPert(engine, pert, raw, from, to, onProgress = () => {}, kill = () => false) {
  engine.set_audio_enabled(0);
  const unmute = muteDrawing(engine);
  try {
    let i = from;
    while (i < to) {
      const t0 = performance.now();
      for (let n = 1; i < to; n++) {
        pert.step(engine, padAt(raw, i));
        i++;
        if ((n & 31) === 0 && performance.now() - t0 > 30) break;
      }
      onProgress(i - from, to - from);
      if (kill()) return false;
      await yieldUi();
    }
    return true;
  } finally { unmute(); }
}
