import React, { useEffect, useRef, useState } from 'react';
import { Link } from 'react-router';
import { GamePad } from 'sm64-binds-frontend';
import { useHub, useVisible } from './useHub.js';
import { loadStart } from './startState.js';
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
  const [info, setInfo] = useState({ text: 'starting the game…', frame: 0, total: 0, star: false, verdict: '' });
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
      setInfo({ text: 'loading the block\'s start state…', frame: 0, total: 0, star: false, verdict: '' });
      let snap;
      try { snap = await loadStart(e, hub, startKind); } catch (x) { setInfo({ text: `no start state: ${x.message || x}`, frame: 0, total: 0, star: false, verdict: '' }); return; }
      if (dead) return;
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
          setInfo({ text: `playing the run being submitted`, frame: 0, total: f.frames, star: false, verdict: '' });
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
            setInfo((x) => ({ ...x, frame: i }));
            if (star) break;
          }
          if (!dead) {
            setInfo({ text: star ? 'STAR!' : 'the run ended', frame: i, total: f.frames, star: star || !!hub.mock, verdict: star ? 'verified in the visual game' : hub.mock ? 'simulated run' : 'the visual game did not reach the goal (check the replay page)' });
            e.set_audio_enabled(0);
          }
        } finally { pert.free(); }
        return;
      }

      // The best path of a worker's search step.
      for (;;) {
        const b = hub.bestPlay(g.w);
        if (!b) { setInfo({ text: 'waiting for the search to find a path…', frame: 0, total: 0, star: false, verdict: '' }); await sleep(1000); if (dead) return; continue; }
        restoreState(e, null, snap);
        let ok = true;
        if (b.played > 0) {
          if (b.prefixRaw) {
            const un = muteDrawing(e); try { await fastForward(e, b.prefixRaw, 0, b.prefixRaw.length / 4, () => {}, () => dead); } finally { un(); }
          } else {
            ok = false;
            setInfo({ text: `step starts at frame ${b.played} of the block (earlier steps' inputs are not in the feed yet): showing the start`, frame: 0, total: 0, star: false, verdict: '' });
          }
        }
        if (ok) {
          const total = b.raw.length / 4;
          let i = 0;
          const myGame = hub.game.id;
          setInfo({ text: `worker ${g.w + 1}'s best path`, frame: 0, total, star: false, verdict: '' });
          while (!dead && i < total && hub.game.id === myGame) {
            await waitVisible();
            let n = await pace();
            while (n-- > 0 && i < total) { e.step_game(padOf(b.raw, i)); i++; }
            setInfo((x) => ({ ...x, frame: i }));
          }
          if (dead || hub.game.id !== myGame) return;
          const s = frameAt();
          const d = b.best ? Math.hypot(s.pos[0] - b.best.pos[0], s.pos[1] - b.best.pos[1], s.pos[2] - b.best.pos[2]) : 0;
          setInfo((x) => ({ ...x, verdict: hub.mock ? 'simulated path' : d < 1 ? 'ends exactly where the search says' : `ends ${d.toFixed(0)} units from the search's position` }));
          await sleep(1200);
        } else await sleep(2000);
        if (dead || hub.game.id !== g.id) return;
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
      <h3>Live game {kind === 'run' ? <span className="chip warn">the run being submitted</span> : <span className="chip">best path of worker {hub.game.w + 1}</span>}</h3>
      <div className="viz-canvaswrap">
        <canvas ref={canvas} className="sm64canvas" />
        {info.star && <div className="starget"><span>★</span><b>STAR GET!</b></div>}
      </div>
      {err && <p className="error">{err}</p>}
      <div className="viz-sub">
        <span>{info.text}</span>
        {info.total > 0 && <span className="muted">{info.frame} / {info.total} frames ({fmtFrames(info.frame)})</span>}
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
