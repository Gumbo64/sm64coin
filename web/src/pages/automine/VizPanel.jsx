import React, { Suspense, useEffect, useState } from 'react';
import './automine.css';
import { useHub } from './useHub.js';
import CampaignStrips from './CampaignStrips.jsx';
import ChainPanel from './ChainPanel.jsx';
import BlocksHistory from './BlocksHistory.jsx';
import EnvelopeView from './EnvelopeView.jsx';
import { miner, hybrid } from '../../lib/webminer/controller.js';
import { useMiner } from '../../lib/webminer/useMiner.js';
import { encPads } from '../../lib/webminer/hybrid.js';

// The Auto-mine page's visualisations (only loaded, and only running, when they are switched on): the
// live game, the 3D search cloud, the envelope, the workers' campaign strips, the chain and the blocks.
const GameView = React.lazy(() => import('./GameView.jsx'));
const CloudView = React.lazy(() => import('./CloudView.jsx'));
const HybridPlay = React.lazy(() => import('./HybridPlay.jsx'));

const VIEWS = [['game', 'Game'], ['cloud', '3D cloud'], ['envelope', 'Envelope'], ['workers', 'Workers'], ['chain', 'Chain'], ['blocks', 'Blocks']];
const load = () => { try { return JSON.parse(localStorage.getItem('sm64coin.vizViews') || 'null') || {}; } catch { return {}; } };

export default function VizPanel({ hub, mode }) {
  const v = useHub(hub);
  void v;
  const { playFromStart, hybridOK } = useMiner();
  // Hybrid mining: {phase: 'asking' | 'playing', sel, msg?, state?} while a person plays from a node.
  const [play, setPlay] = useState(null);
  const [off, setOff] = useState(load);
  const [, tick] = useState(0);
  useEffect(() => { const id = setInterval(() => tick((x) => x + 1), 2000); return () => clearInterval(id); }, []);
  const on = (k) => !off[k];
  const toggle = (k) => setOff((o) => { const n = { ...o, [k]: !o[k] }; try { localStorage.setItem('sm64coin.vizViews', JSON.stringify(n)); } catch { /* private window */ } return n; });
  const toasts = hub.toasts.filter((t) => Date.now() - t.at < 7000);
  const playBest = (w) => { hub.game = { id: hub.game.id + 1, kind: 'best', w }; hub.found = null; hub.notify(); };
  // A node was clicked and "Play from here" pressed: ask the worker's search for its state, then play.
  const playFrom = async (sel) => {
    const w = hub.workers[sel.w];
    if (!w?.cur) return;
    const asked = { sel };
    setPlay({ phase: 'asking', ...asked });
    try {
      const r = await hybrid.requestState(sel.w, sel.id, w.cur.key, 180000);
      setPlay((p) => (p && p.sel === sel && p.phase === 'asking' ? { phase: 'playing', sel, msg: r.msg, state: r.state } : p));
    } catch (e) {
      setPlay((p) => (p && p.sel === sel ? { phase: 'error', sel, text: String(e.message || e) } : p));
    }
  };
  // The segment ended (hotkey): back to the visualiser at once, the segment goes into the search trees meanwhile.
  const played = async (res) => {
    const { sel, msg } = play;
    setPlay(null);
    if (res.discard || res.frames < 1) { hub.toast('segment discarded', 'info'); hub.notify(); return; }
    const info = { w: sel.w, step: sel.step, nid: null, parentSlot: sel.slot, pos: res.pos, level: msg.level, area: msg.area, stars: msg.stars, frame: sel.frame + res.frames, trail: res.trail, frames: res.frames, note: 'adding it to the search tree…' };
    hub.playedSegment(info);
    hub.toast(`played ${res.frames} frames (${(res.frames / 30).toFixed(1)} s) from node ${sel.id}: adding it to the search trees`, 'info');
    let first = true, added = 0;
    const onReply = (slot, m) => {
      if (!m.ok) return;
      added++;
      const own = slot === sel.w;
      if (first || own) {
        first = false;
        const wk = hub.workers[slot];
        const off = Math.hypot(m.pos[0] - res.pos[0], m.pos[1] - res.pos[1], m.pos[2] - res.pos[2]);
        hub.playedSegment({ ...info, w: slot, step: own ? sel.step : wk?.cur?.n ?? sel.step, nid: m.nid, parentSlot: own ? sel.slot : -1, pos: m.pos, level: m.level, area: m.area, stars: m.stars, frame: (wk?.played || 0) + m.frame, trail: res.trail,
          note: `in worker ${slot + 1}'s tree as node ${m.nid} with a high weight${off < 1 ? '; the replay in the search ends exactly where you finished' : `; WARNING: the replay in the search ends ${off.toFixed(1)} units from where you finished`}` });
      }
    };
    try {
      await hybrid.insert({ tag: msg.tag, path: msg.path, seg: encPads(res.pads), weight: 8 }, onReply, 180000);
      hub.toast(`your segment is node of ${added} search tree${added === 1 ? '' : 's'}: the searches start from it first`, 'found');
    } catch (e) {
      hub.playedSegment({ ...info, note: `not added: ${e.message || e}` });
      hub.toast(`the segment was not added: ${e.message || e}`, 'info');
    }
    hub.notify();
  };
  const setGoal = (g) => miner.setGoal(g);
  const fallback = <div className="viz-card"><p className="muted">Loading…</p></div>;
  return (
    <div className="viz">
      <div className="viz-toasts" aria-live="polite">{toasts.map((t) => <div key={t.id} className={`viz-toast ${t.kind}`}>{t.kind === 'found' ? '★ ' : ''}{t.text}</div>)}</div>
      <div className="viz-views">
        <span className="muted small">Views</span>
        {VIEWS.map(([k, l]) => <label key={k}><input type="checkbox" checked={on(k)} onChange={() => toggle(k)} /> {l}</label>)}
      </div>
      <div className="viz-grid2">
        {on('game') && <Suspense fallback={fallback}><GameView hub={hub} /></Suspense>}
        {on('cloud') && <Suspense fallback={fallback}><CloudView hub={hub} onPlay={playBest} onPlayFrom={playFrom} onGoal={setGoal} hybridOK={hybridOK} playFromStart={playFromStart} /></Suspense>}
      </div>
      {on('envelope') && <EnvelopeView hub={hub} />}
      <div className="viz-grid2">
        {on('workers') && <CampaignStrips hub={hub} />}
        {on('blocks') && <BlocksHistory hub={hub} />}
      </div>
      {on('chain') && <ChainPanel hub={hub} mode={mode} />}
      {play && play.phase !== 'playing' && (
        <div className="hy-overlay"><div className="hy-card hy-wait">
          {play.phase === 'asking' && <><h3>Loading node {play.sel.id} of worker {play.sel.w + 1}…</h3><p className="muted">Asking its search for the node's state: it answers between two rounds of its search (a few seconds, longer on a loaded machine).</p></>}
          {play.phase === 'error' && <><h3>Could not play from node {play.sel.id}</h3><p className="error">{play.text}</p></>}
          <button onClick={() => setPlay(null)}>{play.phase === 'asking' ? 'Cancel' : 'Close'}</button>
        </div></div>
      )}
      {play && play.phase === 'playing' && <Suspense fallback={null}><HybridPlay hub={hub} req={play} onDone={played} /></Suspense>}
    </div>
  );
}
