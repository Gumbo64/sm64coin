import React, { useEffect, useRef, useState } from 'react';
import { Controls } from '../Mine.jsx';
import { frames as fmtFrames } from '../../lib/format.js';
import { levelName } from './levels.js';

// Hybrid mining: play by hand from a node of the search tree. The node's save state (the headless game's memory and the
// block's perturbation as the search had them there, from the worker's search: rrt::child::hybrid) is loaded into the visual game
// and played with the manual miner's own code (lib/recorder.js: the pokes of the block's perturbation, rewinds, speeds, the
// same keys and Controls). The attempt never "wins": a hotkey (Enter, or Esc) ends the segment, and its inputs go back to the
// page, which adds them to the search's trees as a manual node (VizPanel.jsx) and returns to the visualiser.
// One visual game serves every visit (a game engine's GL objects belong to its canvas, so it is created once and reattached).

let shared = null; // Promise of {engine, canvas}

function getEngine() {
  shared ??= (async () => {
    const { createEngine } = await import('../../lib/cycle.js');
    const canvas = document.createElement('canvas');
    canvas.className = 'sm64canvas';
    const engine = await createEngine(canvas);
    return { engine, canvas };
  })();
  shared.catch(() => { shared = null; });
  return shared;
}

export default function HybridPlay({ hub, req, onDone }) {
  const slot = useRef(null);
  const rec = useRef(null);
  const stop = useRef(null); // 'finish' | 'discard' once the player ended the segment
  const [status, setStatus] = useState({ text: 'loading the game…' });
  const [clock, setClock] = useState(null);
  const [err, setErr] = useState('');
  const { sel, msg, state } = req;

  useEffect(() => {
    let dead = false;
    (async () => {
      try {
        const [{ engine, canvas }, { Recorder }, { Perturbation, perturbReady, readProgress }, { withConfig }, { restoreState, captureState }, { portableFromRaw }] = await Promise.all([
          getEngine(), import('../../lib/recorder.js'), import('../../lib/perturb.js'), import('../../lib/task.js'), import('../../lib/savestate.js'), import('./startState.js'),
        ]);
        if (dead) return;
        await perturbReady();
        slot.current.appendChild(canvas);
        setStatus({ text: 'loading the node\'s state…' });
        const portable = await portableFromRaw(state.buffer);
        if (dead) return;
        restoreState(engine, null, engine.powerOn);
        engine.load_portable_state(portable);
        const start = { kind: 'node', state: captureState(engine, null, engine.powerOn), progress: readProgress(engine), pertBytes: new TextEncoder().encode(msg.pert) };
        const task = withConfig(hub.template);

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
        setStatus({ text: 'playing', playing: true });
        canvas.focus?.();
        await r.run(async () => !!stop.current || dead);
        rec.current = null;
        if (dead) return;
        const p = engine.get_game_state().pos;
        const k = Math.floor(r.pads.length / 3) + 1;
        const trail = [[msg.pos[0], msg.pos[1], msg.pos[2]], ...r.trail.slice(1, k).filter(Boolean), [p[0], p[1], p[2]]].flat();
        onDone({ discard: stop.current === 'discard', pads: r.pads.map((x) => ({ button: x.button, stick_x: x.stick_x, stick_y: x.stick_y })), frames: r.pads.length, pos: [p[0], p[1], p[2]], trail, task });
      } catch (e) { if (!dead) setErr(String(e.message || e)); }
    })();
    return () => { dead = true; if (slot.current) slot.current.replaceChildren(); };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // The exit hotkey.
  useEffect(() => {
    const onKey = (e) => {
      if (e.key === 'Enter' || e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); stop.current ??= 'finish'; }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, []);

  const used = clock ? clock.frame : 0;
  return (
    <div className="hy-overlay" role="dialog" aria-label="Play from a node">
      <div className="hy-card">
        <h3>Play from node {sel.id} <span className="chip">worker {sel.w + 1}</span> <span className="chip">{levelName(msg.level)} · area {msg.area}</span> <span className="chip">frame {sel.frame} ({fmtFrames(sel.frame)})</span></h3>
        <div ref={slot} className="hy-game" />
        {err && <p className="error">{err}</p>}
        <div className="statusbar">
          <span className="stat" id="hy-status">{status.text === 'playing' ? `playing: ${used} frames played (${fmtFrames(used)})` : status.text}</span>
          {clock && clock.paused && <span className="stat warn">PAUSED</span>}
          {clock && clock.mode === 'catchup' && <span className="stat warn">replaying to frame {clock.target} (T to play from here)</span>}
          <span className="stat">{clock ? `${clock.speed}×` : ''}</span>
        </div>
        <Controls rec={rec} hybrid onFinish={() => { stop.current ??= 'finish'; }} />
        <p className="muted small">
          Play as in the manual miner (WASD stick · I = A · J = B · O = Z · Space = Start · arrows = C; G back to this node, Backspace back 5 s, P pause, 1-6 speed).
          <b> Enter</b> (or Esc) ends the segment: it becomes a node of the search tree with a high weight (the searches start from it first, the preference fading as they expand from it), and you return to the visualiser.{' '}
          <button className="linkish" onClick={() => { stop.current ??= 'discard'; }}>Discard the segment</button>
        </p>
      </div>
    </div>
  );
}
