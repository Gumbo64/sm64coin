import React, { Suspense, useEffect, useState } from 'react';
import './automine.css';
import { useHub } from './useHub.js';
import CampaignStrips from './CampaignStrips.jsx';
import ChainPanel from './ChainPanel.jsx';
import BlocksHistory from './BlocksHistory.jsx';
import EnvelopeView from './EnvelopeView.jsx';

// The Auto-mine page's visualisations (only loaded, and only running, when they are switched on): the
// live game, the 3D search cloud, the envelope, the workers' campaign strips, the chain and the blocks.
const GameView = React.lazy(() => import('./GameView.jsx'));
const CloudView = React.lazy(() => import('./CloudView.jsx'));

const VIEWS = [['game', 'Game'], ['cloud', '3D cloud'], ['envelope', 'Envelope'], ['workers', 'Workers'], ['chain', 'Chain'], ['blocks', 'Blocks']];
const load = () => { try { return JSON.parse(localStorage.getItem('sm64coin.vizViews') || 'null') || {}; } catch { return {}; } };

export default function VizPanel({ hub, mode }) {
  const v = useHub(hub);
  void v;
  const [off, setOff] = useState(load);
  const [, tick] = useState(0);
  useEffect(() => { const id = setInterval(() => tick((x) => x + 1), 2000); return () => clearInterval(id); }, []);
  const on = (k) => !off[k];
  const toggle = (k) => setOff((o) => { const n = { ...o, [k]: !o[k] }; try { localStorage.setItem('sm64coin.vizViews', JSON.stringify(n)); } catch { /* private window */ } return n; });
  const toasts = hub.toasts.filter((t) => Date.now() - t.at < 7000);
  const play = (w, node) => { hub.game = { id: hub.game.id + 1, kind: node ? 'node' : 'best', w, step: node?.step, nid: node?.id }; hub.found = null; hub.notify(); };
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
        {on('cloud') && <Suspense fallback={fallback}><CloudView hub={hub} onPlay={play} /></Suspense>}
      </div>
      {on('envelope') && <EnvelopeView hub={hub} />}
      <div className="viz-grid2">
        {on('workers') && <CampaignStrips hub={hub} />}
        {on('blocks') && <BlocksHistory hub={hub} />}
      </div>
      {on('chain') && <ChainPanel hub={hub} mode={mode} />}
    </div>
  );
}
