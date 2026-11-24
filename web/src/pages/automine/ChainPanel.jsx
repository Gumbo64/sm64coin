import React, { useEffect, useState } from 'react';
import CycleTimeline from '../../lib/CycleTimeline.jsx';
import { secondsUntilAllowed } from '../../lib/resetPolicy.js';
import { useHub } from './useHub.js';

const mmss = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`;

// The chain: the tip and the 70-star cycle (lib/CycleTimeline.jsx, as on the Mine page, with the best search so
// far appended) and the reset-cycle state of the template with its countdown.
export default function ChainPanel({ hub, mode }) {
  const v = useHub(hub);
  void v;
  const [, tick] = useState(0);
  useEffect(() => { const id = setInterval(() => tick((x) => x + 1), 1000); return () => clearInterval(id); }, []);
  const t = hub.template;
  const attempt = hub.workers.reduce((m, w) => (w.best ? Math.max(m, w.best.frameAbs || 0) : m), 0);
  const wait = t ? secondsUntilAllowed(t.sm64_start, t.sm64_power_on_at || 0, Date.now() / 1000) : 0;
  return (
    <div className="viz-card">
      <h3>Chain</h3>
      <CycleTimeline attempt={attempt} limit={t ? t.sm64_max_frames : 0} />
      {t && (
        <div className="viz-sub">
          <span>block {t.height}: parent has <b>{t.sm64_parent_stars} ★</b>{t.sm64_parent_keys ? <> and <b>{t.sm64_parent_keys} 🔑</b></> : null}</span>
          <span>start: <b>{t.sm64_start}</b></span>
          <span className="muted">reset policy: {mode}</span>
          {t.sm64_start === 'either'
            ? <span className="chip warn">reset cycle allowed now</span>
            : <span className="muted">reset cycle allowed in {mmss(wait)} if no block lands</span>}
        </div>
      )}
    </div>
  );
}
