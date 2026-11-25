import React, { useEffect, useState } from 'react';
import './automine.css';
import { useHub } from './useHub.js';
import BlocksHistory from './BlocksHistory.jsx';
import EnvelopeView from './EnvelopeView.jsx';
import LimitChart from '../../lib/LimitChart.jsx';
import { SideCard } from '../../components/ui.jsx';
import { frames as fmtFrames, coins } from '../../lib/format.js';
import { secondsUntilAllowed } from '../../lib/resetPolicy.js';

// The Mine page's auto-mining side cards (only loaded, and only running, when the visualisations are switched on):
// the time-limit chart, the block's numbers, the "Closing in" chart and the blocks mined (VizSide), and the toasts.
const mmss = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`;

export function VizToasts({ hub }) {
  useHub(hub);
  const [, tick] = useState(0);
  useEffect(() => { const id = setInterval(() => tick((x) => x + 1), 2000); return () => clearInterval(id); }, []);
  const toasts = hub.toasts.filter((t) => Date.now() - t.at < 7000);
  return <div className="viz-toasts" aria-live="polite">{toasts.map((t) => <div key={t.id} className={`viz-toast ${t.kind}`}>{t.kind === 'found' ? '★ ' : ''}{t.text}</div>)}</div>;
}

/** The time-limit chart and the block's numbers, from the auto-miner's template. */
export function VizTop({ hub, mode }) {
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
    </>
  );
}

/** The "Closing in" chart and the blocks this session mined. */
export function VizBottom({ hub }) {
  return (
    <>
      <EnvelopeView hub={hub} />
      <BlocksHistory hub={hub} />
    </>
  );
}
