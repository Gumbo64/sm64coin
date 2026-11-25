import React, { useEffect, useRef, useState } from 'react';
import { Link } from 'react-router';
import { GamePad } from 'sm64-binds-frontend';
import { captureState } from '../../lib/savestate.js';
import { useHub, useVisible } from './useHub.js';
import { SPEEDS } from './hub.js';
import { Panel } from '../../components/ui.jsx';
import { loadStart, portableFromRaw } from './startState.js';
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
    if (ready && !gameId) setInfo({ text: 'waiting for the first highlight (a new place or a faster route)…', frame: 0, total: 0, star: false, verdict: '', prog: { frac: null } });
    if (!ready || !gameId) return undefined;
    let dead = false;
    const run = async () => {
      const [{ restoreState }, { Perturbation, perturbReady, readProgress }, { withConfig }, { successFor }, { fastForward }] = await Promise.all([
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
        return () => new Promise((res) => requestAnimationFrame((now) => { owed += Math.min(now - last, 250) * 0.03 * hub.settings.speed; last = now; const n = Math.floor(owed); owed -= n; res(n); }));
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
        let p = g1.kind === 'clip' ? hub.clipPlay(g1.clip) : g1.kind === 'node' ? hub.nodePlay(g1.w, g1.step, g1.nid) : g1.kind === 'best' ? (hub.bestPlay(g1.w) || { wait: 'waiting for the search to find a path…' }) : { wait: hub.reel.on ? 'waiting for the first highlight (a new place or a faster route)…' : 'the highlight reel is paused: pick a node in the cloud' };
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
        const label = g1.kind === 'clip' ? g1.clip.label : g1.kind === 'node' && !said ? `worker ${g1.w + 1}'s node ${g1.nid}` : `worker ${g1.w + 1}'s best path`;
        setInfo({ text: label, frame: 0, total, star: false, verdict: said, prog: { frac: 0 } });
        // Settings: the prefix runs muted at full speed up to `leadSec` before the end (the node, or the clip's highlight); only
        // the lead-in plays at the game's pace (or the whole path does, from the step's start).
        const cs = hub.settings;
        const skipTo = cs.start === 'step' ? 0 : Math.max(0, total - Math.round(cs.leadSec * 30));
        if (skipTo > 0) {
          setInfo((x) => ({ ...x, text: `${label} · skipping ahead…` }));
          const ok = await fastForward(e, p.raw, 0, skipTo, (d) => { i = d; setInfo((x) => ({ ...x, frame: d, prog: { frac: d / total } })); }, () => dead || hub.game.id !== myGame);
          if (!ok || dead || hub.game.id !== myGame) return;
          i = skipTo;
          setInfo((x) => ({ ...x, text: label }));
        }
        while (!dead && i < total && hub.game.id === myGame) {
          await waitVisible();
          let n = await pace();
          while (n-- > 0 && i < total) { e.step_game(padOf(p.raw, i)); i++; }
          setInfo((x) => ({ ...x, frame: i, prog: { frac: total ? i / total : 1 } }));
        }
        if (dead || hub.game.id !== myGame) return;
        const s = frameAt();
        const d = p.best && g1.kind === 'best' ? Math.hypot(s.pos[0] - p.best.pos[0], s.pos[1] - p.best.pos[1], s.pos[2] - p.best.pos[2]) : 0;
        setInfo((x) => ({ ...x, prog: null, verdict: said || (hub.mock ? 'simulated path' : g1.kind === 'node' || g1.kind === 'clip' ? 'reached the node' : d < 1 ? 'ends exactly where the search says' : `ends ${d.toFixed(0)} units from the search's position`) }));
        await sleep(1200);
        if (dead || hub.game.id !== myGame) return;
        // The reel moves on to the next queued clip once this one has played to its end (never mid-play).
        if (hub.reelAdvance()) return;
      }
    };
    run().catch((x) => { if (!dead) setInfo({ text: `game view: ${x.message || x}`, frame: 0, total: 0, star: false, verdict: '' }); });
    return () => { dead = true; };
  }, [ready, gameId]);

  const f = hub.found;
  const shown = kind === 'run' ? f : null;
  void v;
  const st = hub.settings;
  const reel = hub.reel;
  return (
    <Panel className="viz-game" innerRef={box} open={st.open} onToggle={() => hub.setSettings({ open: !st.open })}
      title="Live game" chip={kind === 'run' ? <span className="chip warn">the run being submitted</span> : hub.game.w != null && <span className="chip">{kind === 'clip' ? 'highlight of' : kind === 'node' ? `node ${hub.game.nid} of` : 'best path of'} worker {hub.game.w + 1}</span>}
      settings={(
        <form className="viz-settings" onSubmit={(e) => e.preventDefault()}>
          <span className="lab">Highlight reel</span>
          <div className="ctl">
            <button type="button" onClick={() => hub.setReel({ on: !reel.on })} title="The reel plays what the workers find: a new place, or a faster route to a place. A clip always plays to its end.">{reel.on ? '⏸ Pause' : '▶ Resume'}</button>
            <span className="muted">{reel.queue.length ? `${reel.queue.length} clip${reel.queue.length === 1 ? '' : 's'} queued` : 'no clips queued'} · {reel.clips} so far</span>
          </div>
          <span className="lab">Start point</span>
          <div className="ctl">
            <select value={st.start} onChange={(e) => hub.setSettings({ start: e.target.value })} aria-label="Start point">
              <option value="end">from some seconds before the end</option>
              <option value="step">from the step's start</option>
            </select>
            <label>seconds <input type="number" min="0" max="120" step="1" value={st.leadSec} disabled={st.start !== 'end'} onChange={(e) => hub.setSettings({ leadSec: Math.max(0, Math.min(120, +e.target.value || 0)) })} /></label>
          </div>
          <span className="lab">Playback speed</span>
          <div className="viz-btns">{SPEEDS.map((x) => <button type="button" key={x} className={st.speed === x ? 'on' : ''} onClick={() => hub.setSettings({ speed: x })}>{x}×</button>)}</div>
          <span className="lab">Highlights</span>
          <div className="ctl">
            <label><input type="checkbox" checked={st.places} onChange={(e) => hub.setSettings({ places: e.target.checked })} /> new places</label>
            <label><input type="checkbox" checked={st.records} onChange={(e) => hub.setSettings({ records: e.target.checked })} /> records</label>
          </div>
          <label htmlFor="reel-gain">Smallest record</label>
          <div className="ctl"><input id="reel-gain" type="number" min="10" max="2000" step="5" value={st.minGain} disabled={!st.records} onChange={(e) => hub.setSettings({ minGain: Math.max(10, +e.target.value || 10) })} /> <span className="muted">frames saved (the workers report 10 and up)</span></div>
          <label htmlFor="reel-follow">Follow</label>
          <div className="ctl">
            <select id="reel-follow" value={reel.follow ?? ''} onChange={(e) => hub.setReel({ follow: e.target.value === '' ? null : +e.target.value })}>
              <option value="">all workers</option>
              {hub.workers.map((w) => <option key={w.i} value={w.i}>worker {w.i + 1}</option>)}
            </select>
          </div>
        </form>
      )}>
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
      <div className="viz-status">
        <span className={info.error ? 'error' : ''}>{info.text}{info.total > 0 && <span className="muted"> · {info.frame.toLocaleString()} / {info.total.toLocaleString()} frames</span>}{info.verdict && <span className="muted"> · {info.verdict}</span>}</span>
        {shown && <span>run {shown.frames} frames: <b>{shown.status}</b>{shown.status === 'accepted' && <> · <Link to={`/block/${shown.height}`}>block {shown.height}'s replay →</Link></>}</span>}
      </div>
    </Panel>
  );
}
