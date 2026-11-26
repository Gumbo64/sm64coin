import React, { useState } from 'react';
import { maxFrames, MAX_RUN_FRAMES, MIN_RUN_FRAMES } from './rules.js';

// The frame-length auction for the block being mined: the time limit growing
// from the parent's timestamp (the valid region under it), now, the run in
// progress, and the runs you finished (dots) with their envelope (the best so
// far). A block is valid where the envelope meets the limit.
// Drawn for the Mine page's side column (about 310 px wide), so text stays at its size.
const W = 320, H = 180, L = 44, R = 8, T = 8, B = 24;
const mmss = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`;
// Axis labels: m:ss under an hour, then h:mm ("5:48" would read as minutes).
const tickLabel = (s, step) => (step >= 3600 || s >= 3600 ? `${Math.floor(s / 3600)}h${String(Math.floor((s % 3600) / 60)).padStart(2, '0')}` : mmss(s));
const fmin = (f) => `${(f / 1800).toFixed(f < 1800 ? 1 : 0)} min`;

// Block page use (a finished block, no clock): pass `now={null}` and `title`; `shift` moves the
// curve right (a power-on restart is judged against the limit that many seconds earlier), `groups`
// (lib/blockRuns.js groupRuns) draws one dot per group, the winner's ringed, and `onDot(group)`
// makes dots clickable (`selected`: the group being looked at).
export default function LimitChart({ difficulty, parentTs, now, used, runs, title = 'Time limit', shift = 0, groups = null, onDot = null, selected = null }) {
  const [hover, setHover] = useState(null);
  if (!difficulty || !parentTs) return null;
  const fixed = now == null;
  const elapsed = fixed ? 0 : Math.max(0, now - parentTs);
  const lim = (t) => maxFrames(difficulty, t - shift);
  // Where the limit reaches its cap (bisection on the monotone limit).
  let lo = 0, hi = 1;
  while (lim(hi) < MAX_RUN_FRAMES && hi < 1e7) hi *= 2;
  while (hi - lo > 1) { const m = Math.floor((lo + hi) / 2); if (lim(m) >= MAX_RUN_FRAMES) hi = m; else lo = m; }
  const tCap = hi;
  // A block (fixed) shows the curve out to just past its last run; the Mine page's also past the cap.
  const tMax = fixed ? Math.max(120, ...runs.map((r) => r.t * 1.3)) : Math.max(120, elapsed * 1.08, tCap * 1.15, ...runs.map((r) => r.t * 1.08));
  const yLo = Math.log(MIN_RUN_FRAMES), yHi = Math.log(MAX_RUN_FRAMES * 1.15);
  const x = (t) => L + (t / tMax) * (W - L - R);
  const y = (f) => T + (1 - (Math.log(Math.max(f, MIN_RUN_FRAMES)) - yLo) / (yHi - yLo)) * (H - T - B);
  const N = 160;
  const pts = Array.from({ length: N + 1 }, (_, i) => { const t = (i / N) * tMax; return [t, lim(t)]; });
  const line = pts.map(([t, f], i) => `${i ? 'L' : 'M'}${x(t).toFixed(1)},${y(f).toFixed(1)}`).join('');
  const area = `${line}L${x(tMax).toFixed(1)},${y(MIN_RUN_FRAMES)}L${x(0)},${y(MIN_RUN_FRAMES)}Z`;
  // The envelope: the running minimum of the finished runs, as a step line.
  const sorted = [...runs].sort((a, b) => a.t - b.t);
  let best = Infinity, env = '';
  for (const r of sorted) {
    if (r.frames < best) { env += best === Infinity ? `M${x(r.t)},${y(r.frames)}` : `L${x(r.t)},${y(best)}L${x(r.t)},${y(r.frames)}`; best = r.frames; }
  }
  if (best !== Infinity && !fixed) env += `L${x(Math.min(elapsed, tMax))},${y(best)}`;
  const yTicks = [600, 1800, 5400, 18000, 36000];
  // At most ~5 labels whatever the span (the chart is ~270 px wide; long gaps since the parent run to hours).
  const xStep = [60, 120, 300, 600, 1200, 1800, 3600, 7200, 10800, 21600, 43200, 86400].find((s) => tMax / s <= 5) ?? 86400 * Math.ceil(tMax / 86400 / 5);
  const xTicks = Array.from({ length: Math.floor(tMax / xStep) + 1 }, (_, i) => i * xStep);
  const limitNow = lim(elapsed);
  const onMove = (e) => {
    const box = e.currentTarget.getBoundingClientRect();
    const px = ((e.clientX - box.left) / box.width) * W;
    const t = Math.min(tMax, Math.max(0, ((px - L) / (W - L - R)) * tMax));
    setHover({ t, f: lim(t) });
  };
  return (
    <div className="limitchart">
      <div className="lc-head">
        <b>{title}</b> {!fixed && <span className="muted">(the shortest run is valid first)</span>}
        <span className="lc-legend">
          <span><i className="sw lim" />limit</span>
          <span><i className="sw run" />{fixed ? 'runs' : 'your finished runs'}</span>
          {fixed ? <span><i className="sw win" />winner</span> : <span><i className="sw now" />now</span>}
        </span>
      </div>
      <svg viewBox={`0 0 ${W} ${H}`} onMouseMove={onMove} onMouseLeave={() => setHover(null)} role="img"
        aria-label={fixed ? `Time limit at difficulty ${difficulty} with the runs of this block's height` : `Time limit now ${fmin(limitNow)} at difficulty ${difficulty}, ${mmss(elapsed)} after the last block`}>
        {yTicks.map((f) => <g key={f}><line x1={L} x2={W - R} y1={y(f)} y2={y(f)} className="grid" /><text x={L - 6} y={y(f) + 4} textAnchor="end" className="tick">{fmin(f)}</text></g>)}
        {xTicks.map((t) => <text key={t} x={x(t)} y={H - 8} textAnchor="middle" className="tick">{tickLabel(t, xStep)}</text>)}
        <path d={area} className="valid" />
        <path d={line} className="lim" />
        {!fixed && <line x1={x(Math.min(elapsed, tMax))} x2={x(Math.min(elapsed, tMax))} y1={T} y2={H - B} className="now" />}
        {used > 0 && <line x1={x(Math.min(elapsed, tMax)) - 10} x2={x(Math.min(elapsed, tMax)) + 10} y1={y(used)} y2={y(used)} className="cur"><title>{`your run so far: ${used} frames`}</title></line>}
        {env && <path d={env} className="env" />}
        {groups && groups.map((g, i) => {
          const sel = selected === g;
          const names = g.runs.map((r) => (r.winner ? 'winner' : r.source === 'alt' ? 'sibling block' : 'included run')).join(', ');
          return (
            <g key={i} className={`dotg${onDot ? ' click' : ''}${sel ? ' sel' : ''}`} onClick={onDot ? () => onDot(g) : undefined}
              onKeyDown={onDot ? (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); onDot(g); } } : undefined}
              tabIndex={onDot ? 0 : undefined} role={onDot ? 'button' : undefined} aria-label={`${names}: ${g.frames} frames, ${mmss(g.t)} after the parent`}>
              <circle cx={x(g.t)} cy={y(g.frames)} r={14} className="hit" />
              {g.winner && <circle cx={x(g.t)} cy={y(g.frames)} r={9} className="ring" />}
              <circle cx={x(g.t)} cy={y(g.frames)} r={5} className="dot"><title>{`${names}: ${g.frames} frames (${fmin(g.frames)}), ${mmss(g.t)} after the parent${g.runs.length > 1 ? `; ${g.runs.length} runs here` : ''}`}</title></circle>
              {g.runs.length > 1 && <text x={x(g.t) + 9} y={y(g.frames) - 7} className="tick">{g.runs.length}</text>}
            </g>
          );
        })}
        {!groups && sorted.map((r, i) => <circle key={i} cx={x(r.t)} cy={y(r.frames)} r={5} className="dot"><title>{`run of ${r.frames} frames (${fmin(r.frames)}), finished ${mmss(r.t)} after the last block`}</title></circle>)}
        {hover && <g><line x1={x(hover.t)} x2={x(hover.t)} y1={T} y2={H - B} className="cross" /><circle cx={x(hover.t)} cy={y(hover.f)} r={3.5} className="lim-pt" /></g>}
      </svg>
      <div className="muted lc-foot">
        {fixed ? (hover ? `${mmss(hover.t)} after the parent: limit ${hover.f} frames (${fmin(hover.f)})` : `difficulty ${difficulty}; the limit at each moment after the parent${shift ? ` (a restart from power-on: ${mmss(shift)} later)` : ''}`)
          : hover ? `${mmss(hover.t)} after the last block: limit ${hover.f} frames (${fmin(hover.f)})`
          : `now ${mmss(elapsed)} after the last block: limit ${limitNow} frames (${fmin(limitNow)}), difficulty ${difficulty}${elapsed >= tCap ? ', at its 20-minute cap' : `, cap in ${mmss(tCap - elapsed)}`}`}
      </div>
    </div>
  );
}
