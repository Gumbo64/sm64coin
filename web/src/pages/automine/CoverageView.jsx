import React, { useEffect, useRef, useState } from 'react';
import { useHub, useVisible } from './useHub.js';
import { workerColor } from './levels.js';
import { BUCKET, BUCKET_COLORS } from './coverage.js';

// The sidebar's "Places reached" chart: y = distinct floors the searches reached in the step (the highlight reel's floors:
// coplanar pieces are one place), x = search time (m:ss since the round's first sample). The thick line is the union over the
// workers on the chart's course (the one most workers are on: workers on other steps don't count, see coverage.js), the thin
// ones are each of their own counts; "overlap" = union / the sum of their counts (near 1: the workers explore different
// places, low: they duplicate each other). Under the plot a strip, one cell per ~15 s: new places (new
// floors keep appearing), faster routes (few new floors, but a known spot reached >= 10 frames sooner), no progress (neither), no data
// (no worker sample arrived in the slice). A cyan marker is where a worker found a run: from there the workers restart to beat it (a cyan line under the strip). Dashed lines mark a change of course (its icon).
// (The time-limit chart with the workers' finished runs is lib/LimitChart.jsx, drawn by VizPanel's VizTop.)
const W = 340, H = 236, L = 36, R = 8, T = 10, STRIP = 10, B = 22;
const mmss = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`;
const LEGEND = [['new', 'new places', 'new floors keep appearing in this slice'], ['faster', 'faster routes', 'few new floors, but a known spot was reached at least 10 frames sooner'], ['noprogress', 'no progress', 'neither new floors nor faster routes'], ['none', 'no data', 'no worker samples arrived in this slice']];

export default function CoverageView({ hub }) {
  const v = useHub(hub);
  const box = useRef(null);
  const cv = useRef(null);
  const visible = useVisible(box);
  const [hover, setHover] = useState(null);
  const cov = hub.cover;
  const series = cov.series;
  const last = series[series.length - 1];
  const tMax = Math.max(60, last ? last.t * 1.04 : 0);
  const yMax = Math.max(5, ...series.map((s) => s.union)) * 1.1;
  const plotB = H - B - STRIP - 4;
  const x = (t) => L + (t / tMax) * (W - L - R);
  const y = (n) => T + (1 - n / yMax) * (plotB - T);

  useEffect(() => {
    if (!visible) return;
    const c = cv.current;
    if (!c) return;
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    c.width = W * dpr; c.height = H * dpr;
    const g = c.getContext('2d');
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, W, H);
    g.font = '10px system-ui, sans-serif';
    // y grid
    const ystep = [1, 2, 5, 10, 20, 50, 100, 200, 500, 1000].find((s) => yMax / s <= 5) || 1000;
    g.strokeStyle = 'rgba(128,140,170,.25)'; g.fillStyle = '#8b93a7'; g.lineWidth = 1; g.textAlign = 'right';
    for (let n = 0; n <= yMax; n += ystep) { g.beginPath(); g.moveTo(L, y(n)); g.lineTo(W - R, y(n)); g.stroke(); g.fillText(`${n}`, L - 5, y(n) + 3); }
    g.save(); g.translate(9, (T + plotB) / 2); g.rotate(-Math.PI / 2); g.textAlign = 'center'; g.fillText('floors reached', 0, 0); g.restore();
    // x labels: at most 5
    const steps = [15, 30, 60, 120, 300, 600, 1200, 1800, 3600];
    const xs = steps.find((s) => tMax / s <= 5) || 3600;
    g.textAlign = 'center';
    for (let t = 0; t <= tMax; t += xs) g.fillText(mmss(t), x(t), H - 6);
    // step changes
    g.setLineDash([3, 3]); g.strokeStyle = 'rgba(200,210,235,.45)';
    for (const m of cov.markers) { g.beginPath(); g.moveTo(x(m.t), T); g.lineTo(x(m.t), plotB + STRIP + 4); g.stroke(); g.fillStyle = '#aab3c8'; g.textAlign = 'left'; g.fillText(m.icon || m.label, x(m.t) + 2, T + 8); }
    g.setLineDash([]);
    // lines (broken at step changes)
    const line = (val, color, w) => {
      g.strokeStyle = color; g.lineWidth = w; g.beginPath();
      let pk = null;
      // (a worker not on the chart's place has no value: its line breaks there)
      series.forEach((s) => { const v = val(s); if (v == null) { pk = null; return; } const px = x(s.t), py = y(v); if (s.key !== pk) g.moveTo(px, py); else g.lineTo(px, py); pk = s.key; });
      g.stroke();
    };
    const nW = last ? last.per.length : 0;
    g.globalAlpha = 0.8;
    for (let k = 0; k < nW; k++) line((s) => s.per[k], workerColor(k), 1);
    g.globalAlpha = 1;
    line((s) => s.union, '#fff', 2.5);
    if (last) { g.fillStyle = '#fff'; g.beginPath(); g.arc(x(last.t), y(last.union), 3.5, 0, 7); g.fill(); }
    // the status strip
    const bs = cov.buckets();
    const bw = (W - L - R) / (tMax / BUCKET);
    bs.forEach((b) => {
      g.fillStyle = BUCKET_COLORS[b.kind]; g.fillRect(x(b.t0), plotB + 4, Math.max(1, bw - 1), STRIP);
      if (b.beating) { g.fillStyle = '#22d3ee'; g.fillRect(x(b.t0), plotB + 4 + STRIP + 1, Math.max(1, bw - 1), 2); }
    });
    // runs found: a cyan marker
    g.strokeStyle = '#22d3ee'; g.fillStyle = '#22d3ee'; g.lineWidth = 1.5;
    for (const r of cov.runs) { g.beginPath(); g.moveTo(x(r.t), T); g.lineTo(x(r.t), plotB + STRIP + 4); g.stroke(); g.textAlign = 'left'; g.fillText('run found', Math.min(x(r.t) + 3, W - R - 46), T + 20); }
    if (hover) {
      g.strokeStyle = 'rgba(255,255,255,.6)'; g.lineWidth = 1; g.beginPath(); g.moveTo(x(hover.t), T); g.lineTo(x(hover.t), plotB); g.stroke();
      g.fillStyle = '#fff'; g.beginPath(); g.arc(x(hover.t), y(hover.union), 4, 0, 7); g.fill();
    }
  }, [v, visible, series.length, tMax, yMax, hover]); // eslint-disable-line react-hooks/exhaustive-deps

  const onMove = (e) => {
    const r = cv.current.getBoundingClientRect();
    const px = ((e.clientX - r.left) / r.width) * W;
    const t = ((px - L) / (W - L - R)) * tMax;
    let best = null, bd = Infinity;
    for (const s of series) { const d = Math.abs(s.t - t); if (d < bd) { bd = d; best = s; } }
    setHover(best);
  };
  const ov = cov.overlap();
  const bs = cov.buckets();
  const nowKind = bs.length ? bs[bs.length - 1].kind : 'none';
  const word = { new: 'finding new places', faster: 'finding faster routes', noprogress: 'making no progress', none: '' }[nowKind];
  const where = (s) => (s.of > 1 ? `${s.n} of ${s.of} workers on ${s.label}` : s.label);
  return (
    <div className="viz-card" ref={box}>
      <h3>Places reached <span className="muted small">{last ? `${last.union} floors · ${where(last)}${ov != null ? ` · overlap ${ov.toFixed(2)}` : ''}` : 'waiting for samples'}</span></h3>
      <div className="viz-canvaswrap2">
        <canvas ref={cv} style={{ width: '100%', aspectRatio: `${W} / ${H}` }} onMouseMove={onMove} onMouseLeave={() => setHover(null)} />
      </div>
      <div className="small" style={{ display: 'flex', flexWrap: 'wrap', gap: '2px 10px', margin: '4px 0' }}>
        {LEGEND.map(([k, name, t]) => <span key={k} title={t} style={{ borderLeft: `8px solid ${BUCKET_COLORS[k]}`, paddingLeft: 5 }}>{name}</span>)}
        {cov.runs.length > 0 && <span title="A worker found a run; the workers restart to beat it" style={{ borderLeft: '8px solid #22d3ee', paddingLeft: 5 }}>run found / beating it</span>}
      </div>
      <div className="muted small">
        {hover ? `at ${mmss(hover.t)}: ${hover.union} floors in all, ${where(hover)}${hover.n > 1 ? ` (${hover.per.map((n, i) => (n == null ? null : `w${i + 1} ${n}`)).filter(Boolean).join(', ')})` : ''}`
          : last ? `${word ? `now ${word}. ` : ''}The chart follows the course most workers are on. Thick line: those workers together; thin: each of them. Overlap = union / sum of their counts (near 1: they explore different places; shown with 2 or more there). Strip: new places / faster routes / no progress / no data (no samples).${cov.runs.length ? ' Cyan: a run was found, the workers search for a shorter one.' : ''}`
            : 'The workers send their floor counts about once a second.'}
      </div>
    </div>
  );
}
