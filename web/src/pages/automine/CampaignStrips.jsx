import React from 'react';
import { useHub } from './useHub.js';
import { levelIcon, levelName, logIcon, workerColor } from './levels.js';

// One strip per worker: the campaign's steps (castle -> course -> castle -> ...), each with its level icon,
// its budget bar (frames emulated of the step's budget), the star and key count, and the search's speed.

export default function CampaignStrips({ hub }) {
  const v = useHub(hub);
  void v;
  if (!hub.workers.length) return <div className="viz-card"><h3>Workers</h3><p className="muted">No workers running.</p></div>;
  return (
    <div className="viz-card">
      <h3>Workers <span className="muted small">{hub.workers.reduce((s, w) => s + w.fps, 0) > 0 ? `${(hub.workers.reduce((s, w) => s + w.fps, 0) / 1000).toFixed(0)}k frames/s in total` : ''}</span></h3>
      {hub.workers.map((w) => {
        const last = w.steps[w.steps.length - 1];
        const stars = w.best ? w.best.stars : w.starsBase;
        const keys = w.best ? w.best.keys : w.keysBase;
        return (
          <div className="viz-strip" key={w.i}>
            <div className="viz-strip-head">
              <span className="dot" style={{ background: workerColor(w.i) }} />
              <b>worker {w.i + 1}</b>
              <span className={`chip ${w.start === 'power-on' ? 'warn' : ''}`} title="Where this search starts from">{w.start === 'power-on' ? 'power-on (reset cycle)' : 'parent block'}</span>
              <span>{stars} ★{keys ? ` ${keys} 🔑` : ''}</span>
              {w.cur && <span title="The search's current level">{levelIcon(w.cur.step.level)} {levelName(w.cur.step.level)}</span>}
              <span className="muted">{w.fps > 0 ? `${(w.fps / 1000).toFixed(1)}k fr/s` : ''}{w.nodesTotal ? ` · ${w.nodesTotal.toLocaleString()} nodes` : ''}{w.runs ? ` · ${w.runs} search${w.runs === 1 ? '' : 'es'} done` : ''}</span>
            </div>
            <div className="viz-steps">
              {w.steps.length === 0 && <span className="muted small">{w.line}</span>}
              {w.steps.slice(-14).map((s, k, arr) => {
                const active = s === last;
                const used = active && w.cur ? w.cur.sim / 1e6 : s.done ? s.done.mframes : 0;
                const pct = Math.min(100, (used / Math.max(1, s.budget)) * 100);
                return (
                  <div key={s.n} className={`viz-step${active ? ' active' : ''}${s.done ? ' done' : ''}`} title={`step ${s.n}: ${s.name} from frame ${s.frame}; budget ${s.budget} M frames${s.done ? `; finished at frame ${s.done.frame} (+${s.done.plus}), ${s.done.nodes} nodes, ${s.done.secs} s` : ''}`}>
                    <div className="ico">{logIcon(s.name)}</div>
                    <div className="nm">{s.name}</div>
                    <div className="bar"><div style={{ width: `${pct}%`, background: workerColor(w.i) }} /></div>
                    <div className="muted tiny">{s.done ? `+${s.done.plus} fr` : active ? `${used.toFixed(0)}/${s.budget} M` : `${s.budget} M`}{k === arr.length - 1 ? '' : ''}</div>
                  </div>
                );
              })}
            </div>
            {w.steps.length > 0 && <div className="muted small viz-line">{w.line}</div>}
          </div>
        );
      })}
    </div>
  );
}
