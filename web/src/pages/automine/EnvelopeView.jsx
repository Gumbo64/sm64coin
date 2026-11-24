import React, { useEffect, useRef, useState } from 'react';
import LimitChart from '../../lib/LimitChart.jsx';
import { maxFrames } from '../../lib/rules.js';
import { useHub, useVisible } from './useHub.js';
import { workerColor } from './levels.js';
import { frames as fmtFrames } from '../../lib/format.js';

// The envelope views of the manual miner, for the automated one:
//  1. progress vs frames: one dot per sampled search node (colour: worker), a bigger dot per sample of a
//     worker's best, the best envelope (the fewest frames reached for each progress level) and the template's
//     frame limit as a line that grows with time (the block is valid where the envelope meets it);
//  2. the time-limit chart (lib/LimitChart.jsx, as on the Mine page) with the runs the workers finished.

const W = 640, H = 240, L = 40, R = 10, T = 10, B = 26;

export default function EnvelopeView({ hub }) {
  const v = useHub(hub);
  const box = useRef(null);
  const cv = useRef(null);
  const visible = useVisible(box);
  const [hover, setHover] = useState(null);
  const t = hub.template;
  const nowS = Date.now() / 1000;
  const limit = t ? maxFrames(t.sm64_retarget_difficulty, Math.max(0, nowS - t.sm64_parent_timestamp)) : 0;

  // Envelope: for each progress level the fewest frames any worker reached it in.
  const env = [];
  {
    const best = new Map();
    for (const s of hub.samples) if (!best.has(s.p) || s.frame < best.get(s.p).frame) best.set(s.p, s);
    const ps = [...best.keys()].sort((a, b) => b - a);
    let m = Infinity;
    for (const p of ps) { const s = best.get(p); if (s.frame < m) { m = s.frame; env.push(s); } }
    env.sort((a, b) => a.p - b.p);
  }
  const usedFrames = hub.workers.reduce((m, w) => (w.best ? Math.max(m, w.best.frameAbs || 0) : m), 0);
  const pMax = Math.max(1, ...hub.samples.map((s) => s.p)) + 1;
  // The x range fits the data; the limit line is drawn only when it is within reach (else an arrow at the edge).
  const dataMax = Math.max(600, ...hub.samples.map((s) => s.frame), usedFrames);
  const xMax = limit && limit < dataMax * 4 ? Math.max(limit * 1.15, dataMax * 1.1) : dataMax * 1.25;
  const limitFar = limit > xMax;

  useEffect(() => {
    if (!visible) return;
    const c = cv.current;
    if (!c) return;
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    c.width = W * dpr; c.height = H * dpr;
    const g = c.getContext('2d');
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, W, H);
    const x = (f) => L + (f / xMax) * (W - L - R);
    const y = (p) => T + (1 - p / pMax) * (H - T - B);
    g.font = '10px system-ui, sans-serif';
    g.strokeStyle = 'rgba(128,140,170,.25)'; g.fillStyle = '#8b93a7'; g.lineWidth = 1;
    for (let p = 0; p <= pMax; p += Math.max(1, Math.ceil(pMax / 6))) { g.beginPath(); g.moveTo(L, y(p)); g.lineTo(W - R, y(p)); g.stroke(); g.textAlign = 'right'; g.fillText(`${p} ★`, L - 5, y(p) + 3); }
    const step = xMax > 36000 ? 9000 : xMax > 12000 ? 3000 : xMax > 4000 ? 1800 : xMax > 1500 ? 600 : 300;
    for (let f = 0; f <= xMax; f += step) { g.textAlign = 'center'; g.fillText(`${f} fr`, x(f), H - 9); }
    // The valid region is left of the limit.
    if (limit && limitFar) {
      g.fillStyle = '#6ea8fe'; g.textAlign = 'right'; g.fillText(`limit ${fmtFrames(limit)} →`, W - R, T + 10);
    } else if (limit) {
      g.fillStyle = 'rgba(110,168,254,.08)'; g.fillRect(L, T, x(limit) - L, H - T - B);
      g.strokeStyle = '#6ea8fe'; g.lineWidth = 2; g.setLineDash([]);
      g.beginPath(); g.moveTo(x(limit), T); g.lineTo(x(limit), H - B); g.stroke();
      g.fillStyle = '#6ea8fe'; g.textAlign = x(limit) > W - 90 ? 'right' : 'left';
      g.fillText(`limit ${fmtFrames(limit)}`, x(limit) + (g.textAlign === 'right' ? -4 : 4), T + 10);
    }
    // Dots: the sampled nodes (thinned), then the bests.
    const n = hub.count();
    const stride = Math.max(1, Math.floor(n / 6000));
    g.globalAlpha = 0.35;
    for (let i = 0; i < n; i += stride) {
      g.fillStyle = workerColor(hub.worker[i]);
      g.fillRect(x(hub.frame[i]) - 1, y(hub.stars[i] + (hub.workers[hub.worker[i]]?.keysBase || 0)) - 1, 2, 2);
    }
    g.globalAlpha = 1;
    for (const s of hub.samples) { g.fillStyle = workerColor(s.w); g.beginPath(); g.arc(x(s.frame), y(s.p), 3.5, 0, 7); g.fill(); }
    g.strokeStyle = '#fff'; g.lineWidth = 2; g.beginPath();
    env.forEach((s, i) => { if (i) g.lineTo(x(s.frame), y(env[i - 1].p)); g.lineTo(x(s.frame), y(s.p)); if (!i) g.moveTo(x(s.frame), y(s.p)); });
    g.stroke();
    // The run being submitted.
    if (hub.found) { g.fillStyle = '#f7c948'; g.beginPath(); g.arc(x(hub.found.frames), y(pMax - 1), 6, 0, 7); g.fill(); g.fillStyle = '#fff'; g.textAlign = 'right'; g.fillText('run found', x(hub.found.frames) - 9, y(pMax - 1) + 3); }
  }, [v, visible, xMax, pMax, limit, limitFar, hub.samples.length]);

  const onMove = (e) => {
    const r = cv.current.getBoundingClientRect();
    const px = ((e.clientX - r.left) / r.width) * W, py = ((e.clientY - r.top) / r.height) * H;
    const x = (f) => L + (f / xMax) * (W - L - R), y = (p) => T + (1 - p / pMax) * (H - T - B);
    let best = null, bd = 144;
    for (const s of hub.samples) { const d = (x(s.frame) - px) ** 2 + (y(s.p) - py) ** 2; if (d < bd) { bd = d; best = s; } }
    setHover(best);
  };

  return (
    <div className="viz-card" ref={box}>
      <h3>Envelope <span className="muted small">progress vs frames of every worker's search</span></h3>
      <div className="viz-canvaswrap2">
        <canvas ref={cv} style={{ width: '100%', aspectRatio: `${W} / ${H}` }} onMouseMove={onMove} onMouseLeave={() => setHover(null)} />
      </div>
      <div className="muted small">
        {hover ? `worker ${hover.w + 1}: ${hover.p} ★/keys reached at frame ${hover.frame} (${fmtFrames(hover.frame)})`
          : t ? `limit now ${limit} frames (${fmtFrames(limit)}); dots: nodes by worker; large dots: best of each sample; white line: best envelope`
            : 'waiting for a template…'}
      </div>
      {t && <LimitChart difficulty={t.sm64_retarget_difficulty} parentTs={t.sm64_parent_timestamp} now={nowS} used={usedFrames} runs={hub.runs} />}
    </div>
  );
}
