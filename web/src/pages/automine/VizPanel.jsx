import React, { Suspense, useEffect, useState } from 'react';
import './automine.css';
import { useHub } from './useHub.js';
import BlocksHistory from './BlocksHistory.jsx';
import EnvelopeView from './EnvelopeView.jsx';
import LimitChart from '../../lib/LimitChart.jsx';
import { SideCard } from '../../components/ui.jsx';
import { frames as fmtFrames, coins } from '../../lib/format.js';
import { secondsUntilAllowed } from '../../lib/resetPolicy.js';

// The Auto-mine page's visualisations (only loaded, and only running, when they are switched on). Two pieces the page
// composes: VizMain (the main column: the live game, the 3D search cloud below it) and VizSide (the sidebar below the
// control card: the time-limit chart, the block's numbers, the progress chart, the blocks mined).
const GameView = React.lazy(() => import('./GameView.jsx'));
const CloudView = React.lazy(() => import('./CloudView.jsx'));
const mmss = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`;
const fallback = <div className="viz-card"><p className="muted">Loading…</p></div>;

export function VizMain({ hub }) {
  const v = useHub(hub);
  void v;
  const [, tick] = useState(0);
  useEffect(() => { const id = setInterval(() => tick((x) => x + 1), 2000); return () => clearInterval(id); }, []);
  const toasts = hub.toasts.filter((t) => Date.now() - t.at < 7000);
  const play = (w, node) => { hub.game = { id: hub.game.id + 1, kind: node ? 'node' : 'best', w, step: node?.step, nid: node?.id }; hub.found = null; hub.notify(); };
  return (
    <>
      <div className="viz-toasts" aria-live="polite">{toasts.map((t) => <div key={t.id} className={`viz-toast ${t.kind}`}>{t.kind === 'found' ? '★ ' : ''}{t.text}</div>)}</div>
      <Suspense fallback={fallback}><GameView hub={hub} /></Suspense>
      <Suspense fallback={fallback}><CloudView hub={hub} onPlay={play} /></Suspense>
    </>
  );
}

export function VizSide({ hub, mode }) {
  const v = useHub(hub);
  void v;
  const [, tick] = useState(0);
  useEffect(() => { const id = setInterval(() => tick((x) => x + 1), 1000); return () => clearInterval(id); }, []);
  const t = hub.template;
  const now = Date.now() / 1000;
  const wait = t ? secondsUntilAllowed(t.sm64_start, t.sm64_power_on_at || 0, now) : 0;
  const used = hub.workers.reduce((m, w) => (w.best ? Math.max(m, w.best.frameAbs || 0) : m), 0);
  return (
    <>
      {t && <SideCard><LimitChart difficulty={t.sm64_retarget_difficulty} parentTs={t.sm64_parent_timestamp} now={now} used={used} runs={hub.runs} /></SideCard>}
      <SideCard title="Block">
        {t ? (
          <dl className="kv">
            <dt>Block</dt><dd>{t.height}</dd>
            <dt>Difficulty</dt><dd>{t.difficulty}</dd>
            <dt>Time limit</dt><dd>{fmtFrames(t.sm64_max_frames)} ({t.sm64_max_frames} frames)</dd>
            <dt>Parent</dt><dd>{t.sm64_parent_stars} ★{t.sm64_parent_keys ? ` ${t.sm64_parent_keys} 🔑` : ''}</dd>
            <dt>Start</dt><dd>{t.sm64_start}{t.sm64_start === 'either' ? ' (reset cycle allowed now)' : ` · reset cycle in ${mmss(wait)}`} · policy {mode}</dd>
            {t.expected_reward != null && <><dt>Reward</dt><dd>{coins(t.expected_reward)} SM64</dd></>}
          </dl>
        ) : <p className="muted">Getting a block template…</p>}
      </SideCard>
      <EnvelopeView hub={hub} />
      <BlocksHistory hub={hub} />
    </>
  );
}
