import React, { useEffect, useRef, useState } from 'react';
import { maxFrames } from '../../lib/rules.js';
import { useHub, useVisible } from './useHub.js';
import { workerColor, levelName, logIcon } from './levels.js';
import { frames as fmtFrames } from '../../lib/format.js';

// The Auto-mine sidebar's "Closing in on the star" chart: the current search step's sampled nodes, x = frames of the
// block, y = how far the step's target still is (the search's own estimate, in frames of walking: the nearest of the step's target
// objectives, a star or the castle's painting). One dot per sampled node (colour: worker), the white line is
// the front (the least that was left to go by each frame, over all workers), the bands are the campaign's
// steps with the earliest frame each was done at, and the template's frame limit is a line that grows with time
// (the block is valid where a run reaches the star before it). A new step has a new target: the chart resets.
// (The time-limit chart with the workers' finished runs is lib/LimitChart.jsx, drawn by VizPanel's VizSide.)

// Drawn for the sidebar (about 310 px wide; stacked full width on phones): at 640 px wide its 10 px text was scaled to about 5 px.
const W = 340, H = 220, L = 40, R = 8, T = 10, B = 24;
const BAND = ['#94a3b8', '#60a5fa', '#a78bfa', '#4ade80', '#fb923c', '#f472b6'];
const COURSE_LEVEL = [9, 24, 12, 5, 4, 7, 22, 8, 23, 10, 11, 36, 13, 14, 15];
const PAINTING = { bob: 'Bob-omb Battlefield', wf: "Whomp's Fortress", jrb: 'Jolly Roger Bay', ccm: 'Cool, Cool Mountain', bbh: "Big Boo's Haunt", hmc: 'Hazy Maze Cave', lll: 'Lethal Lava Land', ssl: 'Shifting Sand Land', ddd: 'Dire, Dire Docks', sl: "Snowman's Land", wdw: 'Wet-Dry World', ttm: 'Tall, Tall Mountain', thi: 'Tiny-Huge Island', ttc: 'Tick Tock Clock', rr: 'Rainbow Ride' };

/** What the search calls its target (an objective's key) as a label: "Jolly Roger Bay: star 1 · Plunder in the sunken ship". */
export function targetLabel(t) {
  if (!t) return '';
  const k = t.label || '';
  let m = /^Mission (\d+)-(\d+) \((.*)\)$/.exec(k) || /^LoadedStar (\d+)-(\d+)/.exec(k);
  if (m) {
    const where = levelName(COURSE_LEVEL[+m[1] - 1]);
    const nm = m[3] ? m[3].toLowerCase().replace(/^./, (c) => c.toUpperCase()) : '';
    return `${where}: star ${+m[2] + 1}${nm ? ` · '${nm}'` : ''}`;
  }
  m = /^Painting (\w+)/.exec(k);
  if (m) return `painting to ${PAINTING[m[1]] || m[1]}`;
  if (k.startsWith('Star ')) return 'the nearest star in reach';
  if (k.startsWith('StarDoor')) return 'the star door';
  if (k.startsWith('Door')) return 'the door';
  return k;
}

/** The campaign's steps across workers: {n, name, start (earliest frame it began), done (earliest frame one worker finished it, or null)}. */
function ladder(hub, curN) {
  const by = new Map();
  for (const w of hub.workers) {
    for (const s of w.steps) {
      const e = by.get(s.n) || { n: s.n, name: s.name, start: Infinity, done: null };
      e.start = Math.min(e.start, s.frame);
      if (s.done && (e.done == null || s.done.frame < e.done)) e.done = s.done.frame;
      by.set(s.n, e);
    }
  }
  return [...by.values()].sort((a, b) => a.n - b.n).filter((e) => e.n > curN - 5 && e.n <= curN);
}

export default function EnvelopeView({ hub }) {
  const v = useHub(hub);
  const box = useRef(null);
  const cv = useRef(null);
  const visible = useVisible(box);
  const [hover, setHover] = useState(null);
  const t = hub.template;
  const nowS = Date.now() / 1000;
  const limit = t ? maxFrames(t.sm64_retarget_difficulty, Math.max(0, nowS - t.sm64_parent_timestamp)) : 0;

  // The current search step: the one the furthest worker is in; its nodes are the dots of every worker in that step.
  const view = (() => {
    let lead = null;
    for (const w of hub.workers) if (w.cur && (!lead || w.stepNo > lead.stepNo)) lead = w;
    if (!lead) return { pts: [], front: [], target: null, steps: [], stepNo: 0 };
    const key = lead.cur.key;
    const slotOk = new Map(); // worker -> its search-step number, for the workers in this step
    for (const w of hub.workers) if (w.cur && w.cur.key === key) slotOk.set(w.i, w.cur.n & 4095);
    const n = hub.count();
    const pts = [];
    for (let i = 0; i < n; i++) {
      const d = hub.dist[i];
      if (d < 0 || slotOk.get(hub.worker[i]) !== hub.stepOf[i]) continue;
      pts.push({ w: hub.worker[i], frame: hub.frame[i], d, level: hub.level[i], area: hub.area[i] });
    }
    const stride = Math.max(1, Math.floor(pts.length / 6000));
    const sorted = pts.slice().sort((a, b) => a.frame - b.frame);
    const front = [];
    let m = Infinity;
    for (const p of sorted) if (p.d < m) { m = p.d; front.push(p); }
    return { pts: stride > 1 ? pts.filter((_, i) => i % stride === 0) : pts, front, target: lead.cur.target, steps: ladder(hub, lead.stepNo), stepNo: lead.stepNo, key };
  })();
  const usedFrames = hub.workers.reduce((m, w) => (w.best ? Math.max(m, w.best.frameAbs || 0) : m), 0);
  const dMax = Math.max(10, ...view.pts.map((p) => p.d), view.target ? view.target.d0 : 0) * 1.1;
  // The x range fits the data; the limit line is drawn only when it is within reach (else an arrow at the edge).
  const dataMax = Math.max(600, ...view.pts.map((p) => p.frame), usedFrames);
  const xMax = limit && limit < dataMax * 4 ? Math.max(limit * 1.15, dataMax * 1.1) : dataMax * 1.25;
  const limitFar = limit > xMax;
  // A log scale: the last stretch to the star is what matters, and it is short.
  const yOf = (d) => T + (1 - Math.log10(d + 1) / Math.log10(dMax + 1)) * (H - T - B);

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
    g.font = '10px system-ui, sans-serif';
    // The campaign's steps as bands (from where a step began to the earliest frame one worker finished it).
    view.steps.forEach((s, i) => {
      const x0 = x(Math.min(s.start, xMax)), x1 = x(Math.min(s.done == null ? xMax : s.done, xMax));
      g.fillStyle = BAND[s.n % BAND.length]; g.globalAlpha = s.n === view.stepNo ? 0.1 : 0.16;
      g.fillRect(x0, T, Math.max(1, x1 - x0), H - T - B);
      g.globalAlpha = 1;
      if (s.done != null) { g.strokeStyle = BAND[s.n % BAND.length]; g.lineWidth = 1.5; g.beginPath(); g.moveTo(x1, T); g.lineTo(x1, H - B); g.stroke(); }
    });
    g.strokeStyle = 'rgba(128,140,170,.25)'; g.fillStyle = '#8b93a7'; g.lineWidth = 1;
    for (const d of [0, 1, 3, 10, 30, 100, 300, 1000, 3000, 10000]) {
      if (d > dMax) break;
      g.beginPath(); g.moveTo(L, yOf(d)); g.lineTo(W - R, yOf(d)); g.stroke(); g.textAlign = 'right'; g.fillText(`${d}`, L - 5, yOf(d) + 3);
    }
    g.save(); g.translate(10, (T + H - B) / 2); g.rotate(-Math.PI / 2); g.textAlign = 'center'; g.fillText('frames to go', 0, 0); g.restore();
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
    // Dots: the sampled nodes of the current step.
    g.globalAlpha = 0.45;
    for (const p of view.pts) { g.fillStyle = workerColor(p.w); g.fillRect(x(p.frame) - 1, yOf(p.d) - 1, 2.5, 2.5); }
    g.globalAlpha = 1;
    // The front: the least left to go by each frame.
    if (view.front.length) {
      g.strokeStyle = '#fff'; g.lineWidth = 2; g.beginPath();
      view.front.forEach((p, i) => { if (!i) g.moveTo(x(p.frame), yOf(p.d)); else { g.lineTo(x(p.frame), yOf(view.front[i - 1].d)); g.lineTo(x(p.frame), yOf(p.d)); } });
      const last = view.front[view.front.length - 1];
      g.lineTo(x(Math.max(last.frame, dataMax)), yOf(last.d));
      g.stroke();
      g.fillStyle = '#fff'; g.beginPath(); g.arc(x(last.frame), yOf(last.d), 3.5, 0, 7); g.fill();
    }
    // The run being submitted.
    if (hub.found) { g.fillStyle = '#f7c948'; g.beginPath(); g.arc(x(hub.found.frames), yOf(0), 6, 0, 7); g.fill(); g.fillStyle = '#fff'; g.textAlign = 'right'; g.fillText('run found', x(hub.found.frames) - 9, yOf(0) - 6); }
  }, [v, visible, xMax, dMax, limit, limitFar, hub.samples.length]);

  const onMove = (e) => {
    const r = cv.current.getBoundingClientRect();
    const px = ((e.clientX - r.left) / r.width) * W, py = ((e.clientY - r.top) / r.height) * H;
    const x = (f) => L + (f / xMax) * (W - L - R);
    let best = null, bd = 144;
    for (const s of view.pts) { const d = (x(s.frame) - px) ** 2 + (yOf(s.d) - py) ** 2; if (d < bd) { bd = d; best = s; } }
    setHover(best);
  };

  const tl = targetLabel(view.target);
  const frontNow = view.front.length ? view.front[view.front.length - 1] : null;
  return (
    <div className="viz-card" ref={box}>
      <h3>Closing in on the star <span className="muted small">{tl ? `heading for ${tl}` : 'how far the search is from its goal'}</span></h3>
      <div className="viz-canvaswrap2">
        <canvas ref={cv} style={{ width: '100%', aspectRatio: `${W} / ${H}` }} onMouseMove={onMove} onMouseLeave={() => setHover(null)} />
      </div>
      {view.steps.length > 0 && (
        <div className="small" style={{ display: 'flex', flexWrap: 'wrap', gap: '4px 12px', margin: '4px 0' }}>
          {view.steps.map((s) => (
            <span key={s.n} style={{ borderLeft: `4px solid ${BAND[s.n % BAND.length]}`, paddingLeft: 5, opacity: s.n === view.stepNo ? 1 : 0.8 }}>
              {logIcon(s.name)} {s.name} <span className="muted">{s.done != null ? `done at frame ${s.done}` : `from frame ${s.start}`}</span>
            </span>
          ))}
        </div>
      )}
      <div className="muted small">
        {hover ? `worker ${hover.w + 1}: ${Math.round(hover.d)} frames to go at frame ${hover.frame} (${fmtFrames(hover.frame)}) · ${levelName(hover.level)}, area ${hover.area}`
          : !t ? 'waiting for a template…'
            : !view.target ? 'this step has no target to measure against (waiting for samples)'
              : `${frontNow ? `closest so far: ${Math.round(frontNow.d)} frames to go (of ${Math.round(view.target.d0)} at the step's start, frame ${frontNow.frame}); ` : ''}limit now ${limit} frames (${fmtFrames(limit)}); dots: nodes by worker; white line: the front; bands: steps of the campaign`}
      </div>
    </div>
  );
}
