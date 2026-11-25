import { useEffect } from 'react';
import { acquireEngine } from '../../components/EngineSlot.jsx';
import { e2eHook } from '../../lib/e2e.js';

// Hybrid mining: play by hand from a node of the search tree. The node's save state (the headless game's memory and the
// block's perturbation as the search had them there, from the worker's search: rrt::child::hybrid) is loaded into the shared
// visual game and played with the manual miner's own code (lib/recorder.js: the pokes of the block's perturbation, rewinds,
// speeds, the same keys and toolbars, the touch controls). The attempt never "wins": a hotkey (Enter, or Esc) ends the
// segment, and its inputs go back to the page (pages/Mine.jsx), which adds them to the search's trees as a manual node and
// returns to the visualiser. This component renders nothing: it drives the game panel's canvas while mounted.
//
// `rec` (a ref) is the panel's recorder handle, `stop` (a ref) is set to 'finish' / 'discard' by the hotkey and the buttons.

export default function HybridDriver({ hub, engine, req, rec, stop, setClock, setStatus, onDone, onReady = () => {}, onError }) {
  const { sel, state } = req;
  const msg = req.msg;
  useEffect(() => {
    let dead = false;
    let release = null;
    (async () => {
      try {
        const [{ Recorder }, { Perturbation, perturbReady, readProgress }, { withConfig }, { restoreState, captureState }, { portableFromRaw }] = await Promise.all([
          import('../../lib/recorder.js'), import('../../lib/perturb.js'), import('../../lib/task.js'), import('../../lib/savestate.js'), import('./startState.js'),
        ]);
        release = await acquireEngine();
        if (dead) return;
        await perturbReady();
        let start, task;
        if (req.local) {
          // The page rebuilds the node's state itself (localNode.js): no round trip to the search.
          const { buildLocal } = await import('./localNode.js');
          const t0 = performance.now();
          const r = await buildLocal(engine, hub, req.sel, req.local, (i, n) => setStatus(`replaying to the node: ${Math.round((i / n) * 100)}%`), () => dead);
          if (!r) return;
          start = r.start; task = r.task;
          req.msg = r.msg;
          e2eHook('__lastNodeStart', { mode: 'local', ms: Math.round(performance.now() - t0), pert: r.msg.pert, pos: [...engine.get_game_state().pos] });
          onReady(r.msg);
        } else {
          setStatus('loading the node\'s state…');
          const portable = await portableFromRaw(state.buffer);
          if (dead) return;
          restoreState(engine, null, engine.powerOn);
          engine.load_portable_state(portable);
          start = { kind: 'node', state: captureState(engine, null, engine.powerOn), progress: readProgress(engine), pertBytes: new TextEncoder().encode(msg.pert) };
          task = withConfig(hub.template);
          e2eHook('__lastNodeStart', { mode: 'mailbox', pert: msg.pert, pos: [...engine.get_game_state().pos] });
        }

        // The manual miner's recorder, starting from the node: its perturbation is the node's (not a fresh block start), the
        // attempt never wins, and the positions along the way are kept for the trail drawn in the visualiser.
        class NodeRecorder extends Recorder {
          constructor(...a) { super(...a); this.trail = []; }
          reset() {
            if (this.pert) this.pert.free();
            this.pert = new Perturbation(this.task);
            if (this.start.pertBytes) this.pert.loadBytes(this.start.pertBytes);
            restoreState(this.engine, this.pert, { ...this.start.state, pert: null });
            this.snaps = [{ frame: 0, state: { ...this.start.state, pert: this.pert.toBytes() } }];
            this.pads = []; this.poked = []; this.mode = 'play'; this.replayIdx = 0; this.replayTarget = 0;
          }
          playOne(c) {
            const r = super.playOne(c);
            const n = this.pads.length;
            if (n % 3 === 0) { const p = this.engine.get_game_state().pos; this.trail[n / 3] = [p[0], p[1], p[2]]; }
            return r;
          }
        }
        const r = new NodeRecorder(engine, task, start, () => false, setClock);
        r.setLimit(Number.MAX_SAFE_INTEGER);
        rec.current = r;
        setStatus('playing');
        engine.canvas?.focus?.();
        await r.run(async () => !!stop.current || dead);
        rec.current = null;
        if (dead) return;
        const p = engine.get_game_state().pos;
        const k = Math.floor(r.pads.length / 3) + 1;
        const trail = [[req.msg.pos[0], req.msg.pos[1], req.msg.pos[2]], ...r.trail.slice(1, k).filter(Boolean), [p[0], p[1], p[2]]].flat();
        onDone({ discard: stop.current === 'discard', pads: r.pads.map((x) => ({ button: x.button, stick_x: x.stick_x, stick_y: x.stick_y })), frames: r.pads.length, pos: [p[0], p[1], p[2]], trail, task });
      } catch (e) { if (!dead) onError(String(e.message || e)); } finally { if (release) release(); }
    })();
    return () => { dead = true; };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // The exit hotkey (Enter, or Esc).
  useEffect(() => {
    const onKey = (e) => {
      if (e.key === 'Enter' || e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); stop.current ??= 'finish'; }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [stop]);
  void sel;
  return null;
}
