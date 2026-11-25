import React, { useEffect, useRef, useState } from 'react';
import { Link } from 'react-router';
import { GamePad } from 'sm64-binds-frontend';
import { captureState } from '../../lib/savestate.js';
import { useHub, useVisible } from './useHub.js';
import { loadStart, portableFromRaw } from './startState.js';
import { levelName } from './levels.js';
import { frames as fmtFrames } from '../../lib/format.js';

// The live game: a visual engine (the same Sm64VisualEngine the Mine page plays with) replaying
//  * the best path of a worker's current search step (kind 'best'), looping from the block's start
//    whenever the best improves, or
//  * the exact run being submitted (kind 'run'), through the block's perturbation, until the star.
// Rendering pauses when the canvas is off screen or the tab is hidden.

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const padOf = (raw, i) => new GamePad(raw[i * 4] | (raw[i * 4 + 1] << 8), (raw[i * 4 + 2] << 24) >> 24, (raw[i * 4 + 3] << 24) >> 24);

export default function GameView({ hub }) {
  const v = useHub(hub);
  const box = useRef(null);
  const canvas = useRef(null);
  const visible = useVisible(box);
  const visRef = useRef(visible);
  visRef.current = visible;
  const eng = useRef(null);
  const [ready, setReady] = useState(false);
  const [err, setErr] = useState('');
  const [slow, setSlow] = useState(false);
  useEffect(() => { const t = setTimeout(() => setSlow(true), 15000); return () => clearTimeout(t); }, []);
  const [info, setInfo] = useState({ text: 'loading the game…', frame: 0, total: 0, star: false, verdict: '', prog: { frac: null } });
  const gameId = hub.game.id;
  const kind = hub.game.kind;

  useEffect(() => {
    let dead = false;
    (async () => {
      try {
        const { createEngine } = await import('../../lib/cycle.js');
        const e = await createEngine(canvas.current);
        if (dead) { e.set_audio_enabled(0); return; }
        eng.current = e;
        setReady(true);
      } catch (e) { setErr(String(e.message || e)); }
    })();
    return () => { dead = true; if (eng.current) { try { eng.current.set_audio_enabled(0); } catch { /* gone */ } } eng.current = null; };
  }, []);

  useEffect(() => {
    if (ready && !gameId) setInfo({ text: 'waiting for the search to find a path…', frame: 0, total: 0, star: false, verdict: '', prog: { frac: null } });
    if (!ready || !gameId) return undefined;
    let dead = false;
    const run = async () => {
      const [{ restoreState }, { Perturbation, perturbReady, readProgress }, { withConfig }, { successFor }, { fastForward, muteDrawing }] = await Promise.all([
        import('../../lib/savestate.js'), import('../../lib/perturb.js'), import('../../lib/task.js'), import('../../lib/rules.js'), import('../../lib/cycle.js'),
      ]);
      await perturbReady();
      const e = eng.current;
      const g = hub.game;
      const isRun = g.kind === 'run';
      const t = isRun ? hub.found.template : hub.template;
      const startKind = isRun ? (hub.found.power ? 'power-on' : 'parent') : hub.workers[g.w]?.start;
      let snap = null;
      const blockStart = async () => (snap ??= await loadStart(e, hub, startKind));
      if (isRun) {
        setInfo({ text: 'loading the block\'s start state…', frame: 0, total: 0, star: false, verdict: '', prog: { frac: null } });
        try { await blockStart(); } catch (x) { setInfo({ text: `no start state: ${x.message || x}`, frame: 0, total: 0, star: false, verdict: '', prog: null }); return; }
        if (dead) return;
      }
      const frameAt = () => {
        const s = e.get_game_state();
        return s;
      };
      const waitVisible = async () => { while (!dead && !visRef.current) await sleep(300); };
      // 30 fps game clock on top of requestAnimationFrame.
      const pace = (() => {
        let last = performance.now(), owed = 0;
        return () => new Promise((res) => requestAnimationFrame((now) => { owed += Math.min(now - last, 250) * 0.03; last = now; const n = Math.floor(owed); owed -= n; res(n); }));
      })();

      if (isRun) {
        const f = hub.found;
        if (!f.inputs) { setInfo({ text: 'the run\'s proof could not be read', frame: 0, total: 0, star: false, verdict: '' }); return; }
        const won = successFor(t.sm64_rules);
        const task = withConfig(t);
        const startProgress = readProgress(e);
        const pert = new Perturbation(task);
        try {
          let i = 0, k = 0, star = false;
          setInfo({ text: `playing the run being submitted`, frame: 0, total: f.frames, star: false, verdict: '', prog: { frac: 0 } });
          while (!dead && i < f.frames) {
            await waitVisible();
            let n = await pace();
            while (n-- > 0 && i < f.frames) {
              const wanted = f.inputs[k] || { button: 0, stick_x: 0, stick_y: 0 };
              const [played, poked] = pert.step(e, new GamePad(wanted.button, wanted.stick_x, wanted.stick_y));
              if (!poked) k++;
              void played;
              i++;
              if (won && won(startProgress, readProgress(e))) { star = true; break; }
            }
            setInfo((x) => ({ ...x, frame: i, prog: { frac: f.frames ? i / f.frames : 1 } }));
            if (star) break;
          }
          if (!dead) {
            setInfo({ text: star ? 'STAR!' : 'the run ended', frame: i, total: f.frames, star: star || !!hub.mock, prog: null, verdict: star ? 'verified in the visual game' : hub.mock ? 'simulated run' : 'the visual game did not reach the goal (check the replay page)' });
            e.set_audio_enabled(0);
          }
        } finally { pert.free(); }
        return;
      }

      // The best path of a worker's search step, or one of its nodes: the game starts at the step's start state
      // (sent by the worker: no replay from the block's start) and plays the step's own inputs.
      const snaps = new Map();
      const note = (text, extra = {}) => setInfo({ text, frame: 0, total: 0, star: false, verdict: '', prog: null, ...extra });
      for (;;) {
        const g1 = hub.game;
        if (g1.kind === 'run') return;
        let p = g1.kind === 'node' ? hub.nodePlay(g1.w, g1.step, g1.nid) : (hub.bestPlay(g1.w) || { wait: 'waiting for the search to find a path…' });
        let said = '';
        if (p.error) { said = p.error; p = hub.bestPlay(g1.w) || { wait: p.error }; }
        if (!p.raw) { note(p.wait, { prog: { frac: null } }); await sleep(700); if (dead || hub.game.id !== g1.id) return; continue; }
        // The start: the step's start state (converted for the visual game once per step), or the block's start.
        const sk = p.rootRaw ? `${g1.w}:${p.key}` : p.played === 0 ? 'block' : null;
        if (!sk) { note('waiting for the worker to send the step\'s start state…', { prog: { frac: null } }); await sleep(700); if (dead || hub.game.id !== g1.id) return; continue; }
        let st = snaps.get(sk);
        if (!st) {
          try {
            if (p.rootRaw) {
              note('converting the step\'s start state for the game…', { prog: { frac: null } });
              hub.portable ??= {};
              hub.portable[sk] ??= portableFromRaw(p.rootRaw);
              const portable = await hub.portable[sk];
              if (dead) return;
              restoreState(e, null, e.powerOn);
              e.load_portable_state(portable);
              st = captureState(e, null, e.powerOn);
            } else {
              note('loading the block\'s start state…', { prog: { frac: null } });
              st = await blockStart();
              if (dead) return;
            }
          } catch (x) { note(`could not load the start state: ${x.message || x}`, { error: true }); await sleep(3000); if (dead || hub.game.id !== g1.id) return; continue; }
          snaps.clear();
          snaps.set(sk, st);
        }
        restoreState(e, null, st);
        const total = p.raw.length / 4;
        let i = 0;
        const myGame = g1.id;
        const label = g1.kind === 'node' && !said ? `worker ${g1.w + 1}'s node ${g1.nid}` : `worker ${g1.w + 1}'s best path`;
        setInfo({ text: label, frame: 0, total, star: false, verdict: said, prog: { frac: 0 } });
        while (!dead && i < total && hub.game.id === myGame) {
          await waitVisible();
          let n = await pace();
          while (n-- > 0 && i < total) { e.step_game(padOf(p.raw, i)); i++; }
          setInfo((x) => ({ ...x, frame: i, prog: { frac: total ? i / total : 1 } }));
        }
        if (dead || hub.game.id !== myGame) return;
        const s = frameAt();
        const d = p.best && g1.kind !== 'node' ? Math.hypot(s.pos[0] - p.best.pos[0], s.pos[1] - p.best.pos[1], s.pos[2] - p.best.pos[2]) : 0;
        setInfo((x) => ({ ...x, prog: null, verdict: said || (hub.mock ? 'simulated path' : g1.kind === 'node' ? 'reached the node' : d < 1 ? 'ends exactly where the search says' : `ends ${d.toFixed(0)} units from the search's position`) }));
        await sleep(1200);
        if (dead || hub.game.id !== myGame) return;
      }
    };
    run().catch((x) => { if (!dead) setInfo({ text: `game view: ${x.message || x}`, frame: 0, total: 0, star: false, verdict: '' }); });
    return () => { dead = true; };
  }, [ready, gameId]);

  const f = hub.found;
  const shown = kind === 'run' ? f : null;
  void v;
  return (
    <div className="viz-card viz-game" ref={box}>
      <h3>Live game {kind === 'run' ? <span className="chip warn">the run being submitted</span> : hub.game.w != null && <span className="chip">{kind === 'node' ? `node ${hub.game.nid} of` : 'best path of'} worker {hub.game.w + 1}</span>}</h3>
      <div className="viz-canvaswrap">
        <canvas ref={canvas} className="sm64canvas" />
        {info.star && <div className="starget"><span>★</span><b>STAR GET!</b></div>}
      </div>
      {err && <p className="error">The game could not start: {err}{/webgl|context/i.test(err) ? ' (this browser may not allow WebGL here; try another browser or switch off battery saver)' : ''}</p>}
      {!err && !ready && slow && <p className="muted small">Still loading the game engine: slow on phones, the first load can take a minute…</p>}
      {info.prog && !err && (
        <div className="viz-progress" role="progressbar" aria-label={info.text} aria-valuemin={0} aria-valuemax={100} aria-valuenow={info.prog.frac == null ? undefined : Math.round(info.prog.frac * 100)}>
          <div className={info.prog.frac == null ? 'indet' : ''} style={info.prog.frac == null ? undefined : { width: `${Math.round(info.prog.frac * 100)}%` }} />
        </div>
      )}
      <div className="viz-sub">
        <span className={info.error ? 'error' : ''}>{info.text}</span>
        {info.total > 0 && <span className="muted">replaying {info.frame.toLocaleString()} / {info.total.toLocaleString()} frames ({fmtFrames(info.frame)})</span>}
      </div>
      {info.verdict && <div className="muted small">{info.verdict}</div>}
      {shown && (
        <div className="viz-sub">
          <span>run {shown.frames} frames: <b>{shown.status}</b></span>
          {shown.status === 'accepted' && <Link to={`/block/${shown.height}`}>open block {shown.height}'s replay →</Link>}
        </div>
      )}
      {!shown && hub.workers[hub.game.w]?.cur && <div className="muted small">{levelName(hub.workers[hub.game.w].cur.step.level)} · {hub.workers[hub.game.w].cur.step.goal}</div>}
    </div>
  );
}
