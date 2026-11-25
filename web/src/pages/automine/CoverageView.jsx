import React, { useEffect, useRef, useState } from 'react';
import { useHub, useVisible } from './useHub.js';
import { workerColor } from './levels.js';
import { BUCKET, BUCKET_COLORS } from './coverage.js';

// The sidebar's "Places reached" chart: y = distinct floors the searches reached in the step (the highlight reel's floors:
// coplanar pieces are one place), x = search time (m:ss since the round's first sample). The thick line is the union over the
// workers, the thin ones are each worker's own count; "overlap" = union / the sum of the workers' counts (near 1: the workers
// explore different places, low: they duplicate each other). Under the plot a strip, one cell per ~15 s: exploring (new
// floors keep appearing), refining (few new floors, but faster arrivals: records), stuck (neither). Dashed lines mark step changes.
// (The time-limit chart with the workers' finished runs is lib/LimitChart.jsx, drawn by VizPanel's VizTop.)
const W = 340, H = 236, L = 36, R = 8, T = 10, STRIP = 10, B = 22;
const mmss = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`;
const LEGEND = [['exploring', 'new floors keep appearing'], ['refining', 'few new floors, but faster arrivals'], ['stuck', 'neither']];

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
    for (const m of cov.markers) { g.beginPath(); g.moveTo(x(m.t), T); g.lineTo(x(m.t), plotB + STRIP + 4); g.stroke(); g.fillStyle = '#aab3c8'; g.textAlign = 'left'; g.fillText(m.label.replace('step ', 's'), x(m.t) + 2, T + 8); }
    g.setLineDash([]);
    // lines (broken at step changes)
    const line = (val, color, w) => {
      g.strokeStyle = color; g.lineWidth = w; g.beginPath();
      let pk = null;
      series.forEach((s, i) => { const px = x(s.t), py = y(val(s)); if (i === 0 || s.key !== pk) g.moveTo(px, py); else g.lineTo(px, py); pk = s.key; });
      g.stroke();
    };
    const nW = last ? last.per.length : 0;
    g.globalAlpha = 0.8;
    for (let k = 0; k < nW; k++) line((s) => s.per[k] || 0, workerColor(k), 1);
    g.globalAlpha = 1;
    line((s) => s.union, '#fff', 2.5);
    if (last) { g.fillStyle = '#fff'; g.beginPath(); g.arc(x(last.t), y(last.union), 3.5, 0, 7); g.fill(); }
    // the status strip
    const bs = cov.buckets();
    const bw = (W - L - R) / (tMax / BUCKET);
    bs.forEach((b) => { g.fillStyle = BUCKET_COLORS[b.kind]; g.fillRect(x(b.t0), plotB + 4, Math.max(1, bw - 1), STRIP); });
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
  const word = { exploring: 'exploring', refining: 'refining', stuck: 'stuck', none: '' }[nowKind];
  return (
    <div className="viz-card" ref={box}>
      <h3>Places reached <span className="muted small">{last ? `${last.union} floors${ov != null ? ` · overlap ${ov.toFixed(2)}` : ''}` : 'waiting for samples'}</span></h3>
      <div className="viz-canvaswrap2">
        <canvas ref={cv} style={{ width: '100%', aspectRatio: `${W} / ${H}` }} onMouseMove={onMove} onMouseLeave={() => setHover(null)} />
      </div>
      <div className="small" style={{ display: 'flex', flexWrap: 'wrap', gap: '2px 10px', margin: '4px 0' }}>
        {LEGEND.map(([k, t]) => <span key={k} title={t} style={{ borderLeft: `8px solid ${BUCKET_COLORS[k]}`, paddingLeft: 5 }}>{k}</span>)}
      </div>
      <div className="muted small">
        {hover ? `at ${mmss(hover.t)}: ${hover.union} floors in all${hover.per.length > 1 ? ` (${hover.per.map((n, i) => `w${i + 1} ${n}`).join(', ')})` : ''}`
          : last ? `${word ? `now ${word}. ` : ''}Thick line: all workers together; thin: each worker. Overlap = union / sum of the workers' counts (near 1: they explore different places).`
            : 'The workers send their floor counts about once a second.'}
      </div>
    </div>
  );
}
