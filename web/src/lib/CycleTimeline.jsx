import React, { useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from 'react-router';
import { daemon } from './rpc.js';
import { STALL_SECONDS } from './rules.js';

// The current cycle as a timeline, after sm64-viewer's (frontend/src/components/
// Timeline.jsx): the strip shows the cycle's played inputs (stick in grey,
// A / B / Z lanes), the track above it one labelled span per block (what it
// got: "BoB star 6"), and a marker where each star or key was collected.
// `attempt` (frames) appends the run in progress. Clicking a block opens it.

const LANES = [
  { mask: 0x8000, color: '#4ade80' }, // A
  { mask: 0x4000, color: '#60a5fa' }, // B
  { mask: 0x2000, color: '#c084fc' }, // Z
];
const COLORS = ['#f7c948', '#60a5fa', '#4ade80', '#f472b6', '#fb923c', '#a78bfa', '#2dd4bf', '#f87171'];
const COURSES = ['Castle', 'BoB', 'WF', 'JRB', 'CCM', 'BBH', 'HMC', 'LLL', 'SSL', 'DDD', 'SL', 'WDW', 'TTM', 'THI', 'TTC', 'RR',
  'BitDW', 'BitFS', 'BitS', 'PSS', 'CotMC', 'TotWC', 'VCutM', 'WMotR', 'SA', 'Cake'];

/** "c01s6" -> "BoB star 6", "key1" -> "Bowser key 1", "grand" -> "Grand Star". */
export function eventName(e) {
  const m = /^c(\d\d)s(\d)$/.exec(e || '');
  if (m) return `${COURSES[Number(m[1])] || 'course ' + Number(m[1])} ${Number(m[1]) === 0 ? 'secret star' : 'star'} ${m[2]}`;
  if ((e || '').startsWith('key')) return `Bowser key ${e.slice(3)}`;
  if (e === 'grand') return 'Grand Star';
  return e || '?';
}

const fmtTime = (frame) => {
  const s = frame / 30;
  const m = Math.floor(s / 60);
  return `${m}:${(s - m * 60).toFixed(1).padStart(4, '0')}`;
};
const fmtCountdown = (s) => {
  if (s <= 0) return 'now';
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
  return h ? `${h}h ${String(m).padStart(2, '0')}m` : `${m}:${String(r).padStart(2, '0')}`;
};

function hexPads(hex) {
  const pads = new Array(hex.length / 8);
  for (let i = 0; i < pads.length; i++) {
    const o = i * 8;
    const b = (n) => parseInt(hex.substr(o + 2 * n, 2), 16);
    const s8 = (v) => (v > 127 ? v - 256 : v);
    pads[i] = { button: b(0) | (b(1) << 8), stick_x: s8(b(2)), stick_y: s8(b(3)) };
  }
  return pads;
}

/** A get_sm64_cycle reply, with each block's frame span and the pads decoded. */
export function decodeCycle(c) {
  let at = 0;
  const blocks = (c.blocks || []).map((b) => { const r = { ...b, from: at, to: at + b.frames }; at += b.frames; return r; });
  return { ...c, blocks, pads: hexPads(c.played_pads || '') };
}

/** One cycle as given (e.g. a block's), with `highlight` (a block hash) marked
 * and an optional `playhead` (frame within the cycle). */
export function CycleStrip({ cycle, highlight, playhead }) {
  const c = useMemo(() => decodeCycle(cycle), [cycle]);
  return (
    <div className="cycle">
      <div className="cycle-head">
        <b>Cycle from block {c.start_height}</b>
        <span>{c.stars} ★{c.keys ? ` · ${c.keys} 🔑` : ''}{c.grand_star ? ' · Grand Star' : ''}</span>
        <span className="muted">{c.blocks.length} block{c.blocks.length === 1 ? '' : 's'} · {fmtTime(c.pads.length)} of play</span>
      </div>
      <Strip blocks={c.blocks} pads={c.pads} attempt={0} highlight={highlight} playhead={playhead} />
    </div>
  );
}

/** The current cycle (blocks, pads, tip progress), refreshed when a block lands. */
export function useCycle() {
  const [cycle, setCycle] = useState(null);
  const [err, setErr] = useState('');
  useEffect(() => {
    let top = null, stop = false;
    const poll = async () => {
      try {
        const info = await daemon('get_info');
        if (info.top_block_hash === top) return;
        const [c, h] = await Promise.all([
          daemon('get_sm64_cycle', { hash: info.top_block_hash }),
          daemon('get_block_header_by_hash', { hash: info.top_block_hash }),
        ]);
        if (stop) return;
        top = info.top_block_hash;
        setCycle({ ...decodeCycle(c), tipTime: h.block_header.timestamp, tipHeight: h.block_header.height, top });
        setErr('');
      } catch (e) { setErr(String(e.message || e)); }
    };
    poll();
    const id = setInterval(poll, 5000);
    return () => { stop = true; clearInterval(id); };
  }, []);
  return [cycle, err];
}

export default function CycleTimeline({ attempt = 0 }) {
  const [cycle, err] = useCycle();
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => { const id = setInterval(() => setNow(Date.now() / 1000), 1000); return () => clearInterval(id); }, []);
  if (err) return <div className="card muted">Cycle: daemon unreachable ({err})</div>;
  if (!cycle) return <div className="card muted">Loading the cycle…</div>;

  const newCycle = cycle.ends_cycle || cycle.blocks.length === 0;
  const resetIn = Math.ceil(cycle.tipTime + STALL_SECONDS + 1 - now);
  return (
    <div className="card cycle">
      <div className="cycle-head">
        <b>Current cycle</b>
        <span>{cycle.stars} ★{cycle.keys ? ` · ${cycle.keys} 🔑` : ''}{cycle.grand_star ? ' · Grand Star' : ''}</span>
        <span className="muted">{cycle.blocks.length ? `from block ${cycle.start_height} · ${cycle.blocks.length} block${cycle.blocks.length > 1 ? 's' : ''} · ${fmtTime(cycle.pads.length)} of play` : 'no blocks yet'}</span>
        <span className="cycle-reset" title={`After ${STALL_SECONDS / 60} minutes without a block, a block may start over from power-on (the escape from a stuck game).`}>
          {newCycle
            ? 'Next block starts a new cycle from power-on'
            : resetIn > 0 ? <>Reset to power-on allowed in <b>{fmtCountdown(resetIn)}</b></> : <>Reset to power-on <b>allowed now</b> (the chain has stalled)</>}
        </span>
      </div>
      {cycle.blocks.length > 0 || attempt > 0
        ? <Strip blocks={cycle.blocks} pads={cycle.pads} attempt={attempt} />
        : <p className="muted">The first block of the cycle starts from power-on: get any star.</p>}
    </div>
  );
}

function Strip({ blocks, pads, attempt, highlight, playhead }) {
  const nav = useNavigate();
  const wrap = useRef(null);
  const strip = useRef(null);
  const [width, setWidth] = useState(800);
  const [hover, setHover] = useState(null);
  const total = pads.length + attempt;

  useEffect(() => {
    const ro = new ResizeObserver(([e]) => setWidth(Math.max(100, Math.floor(e.contentRect.width))));
    if (wrap.current) ro.observe(wrap.current);
    return () => ro.disconnect();
  }, []);

  const columns = useMemo(() => {
    const cols = [];
    for (let x = 0; x < width; x++) {
      const a = Math.floor((x / width) * total);
      const b = Math.max(a + 1, Math.floor(((x + 1) / width) * total));
      let stick = 0;
      const lanes = LANES.map(() => 0);
      for (let i = a; i < b && i < pads.length; i++) {
        const p = pads[i];
        stick = Math.max(stick, Math.hypot(p.stick_x, p.stick_y) / 128);
        LANES.forEach((l, k) => { if (p.button & l.mask) lanes[k] += 1; });
      }
      cols.push({ stick, lanes: lanes.map((c) => c / Math.max(1, b - a)) });
    }
    return cols;
  }, [pads, width, total]);

  useEffect(() => {
    const c = strip.current;
    if (!c) return;
    const dpr = window.devicePixelRatio || 1;
    const h = 36;
    c.width = width * dpr;
    c.height = h * dpr;
    const g = c.getContext('2d');
    g.scale(dpr, dpr);
    g.clearRect(0, 0, width, h);
    columns.forEach((col, x) => {
      const sh = col.stick * 18;
      g.fillStyle = '#3f4656';
      g.fillRect(x, 18 - sh, 1, sh);
      col.lanes.forEach((v, k) => {
        if (v > 0) {
          g.globalAlpha = 0.35 + 0.65 * Math.min(1, v * 3);
          g.fillStyle = LANES[k].color;
          g.fillRect(x, 21 + k * 5, 1, 4);
          g.globalAlpha = 1;
        }
      });
    });
  }, [columns, width]);

  const pct = (f) => `${(100 * f) / Math.max(1, total)}%`;
  const frameAt = (e) => {
    const r = wrap.current.getBoundingClientRect();
    return Math.round(Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)) * total);
  };
  const blockAt = (f) => blocks.find((b) => f >= b.from && f < b.to);
  const hovered = hover !== null ? blockAt(hover) : null;

  return (
    <div className="timeline-wrap">
      <div className="region-track">
        {blocks.map((b, i) => (
          <button
            key={b.hash}
            className={`region${b.hash === highlight ? ' current' : ''}`}
            style={{ left: pct(b.from), width: pct(b.frames), '--region': COLORS[i % COLORS.length] }}
            title={`Block ${b.height}: ${eventName(b.event)} in ${fmtTime(b.frames)}${b.power_on ? ' (from power-on)' : ''}`}
            onClick={() => nav(`/block/${b.hash}`)}
          >
            <span>{eventName(b.event)}</span>
          </button>
        ))}
        {attempt > 0 && (
          <div className="region current attempt" style={{ left: pct(pads.length), width: pct(attempt), '--region': '#e8eaf6' }} title="Your attempt in progress">
            <span>you</span>
          </div>
        )}
      </div>
      <div
        className="timeline clickable"
        ref={wrap}
        onMouseMove={(e) => setHover(frameAt(e))}
        onMouseLeave={() => setHover(null)}
        onClick={(e) => {
          // Open the replay of the block under the click, at that frame
          // (frames count from the cycle's start, as on the block page).
          const f = frameAt(e);
          const at = blockAt(f);
          if (at) nav(`/block/${at.hash}?f=${f}`);
        }}
        title="Click to watch from here"
      >
        <canvas ref={strip} className="strip" style={{ width: '100%', height: 36 }} />
        <div className="played" style={{ width: pct(pads.length) }} />
        {blocks.map((b) => (
          <div key={b.hash} className="marker" style={{ left: pct(b.to) }} title={`${eventName(b.event)} (block ${b.height})`} />
        ))}
        {attempt > 0 && <div className="playhead" style={{ left: pct(total) }} />}
        {playhead != null && <div className="playhead" style={{ left: pct(playhead) }} />}
        {hover !== null && (
          <div className="hover" style={{ left: pct(hover) }}>
            {fmtTime(hover)}{hovered ? ` · block ${hovered.height}: ${eventName(hovered.event)}` : hover >= pads.length && attempt ? ' · your attempt' : ''}
          </div>
        )}
      </div>
    </div>
  );
}

const CYCLES_SHOWN = 6;
const doneCycles = new Map(); // first block hash -> finished cycle summary (never changes)

function summary(c, current) {
  const first = c.blocks[0], last = c.blocks[c.blocks.length - 1];
  return {
    first: first.hash, start: first.height, end: last.height, blocks: c.blocks.length,
    stars: c.stars, keys: c.keys, grand: c.grand_star, ends: c.ends_cycle,
    frames: c.blocks.reduce((s, b) => s + b.frames, 0), current,
  };
}

/** The most recent cycles, newest first (walking back with get_sm64_cycle). */
export function RecentCycles() {
  const nav = useNavigate();
  const [cycles, setCycles] = useState(null);
  useEffect(() => {
    let top = null, stop = false;
    const poll = async () => {
      try {
        const info = await daemon('get_info');
        if (info.top_block_hash === top) return;
        const out = [];
        let hash = info.top_block_hash;
        for (let i = 0; i < CYCLES_SHOWN && hash; i++) {
          let s = i > 0 ? [...doneCycles.values()].find((d) => d.endHash === hash) : null;
          if (!s) {
            const c = await daemon('get_sm64_cycle', { hash });
            c.blocks = c.blocks || []; // the RPC omits an empty list
            if (!c.blocks.length) break; // reached the genesis block
            s = { ...summary(c, i === 0), endHash: hash };
            const h = await daemon('get_block_header_by_hash', { hash: c.blocks[0].hash });
            s.prev = h.block_header.height > 1 ? h.block_header.prev_hash : null;
            if (i > 0) doneCycles.set(s.first, s);
          }
          out.push({ ...s, current: i === 0 });
          hash = s.prev;
        }
        if (!stop) { top = info.top_block_hash; setCycles(out); }
      } catch { /* keep the last list */ }
    };
    poll();
    const id = setInterval(poll, 10000);
    return () => { stop = true; clearInterval(id); };
  }, []);
  return (
    <aside className="card cycles">
      <h3>Recent cycles</h3>
      {!cycles ? <p className="muted">Loading…</p> : cycles.length === 0 ? <p className="muted">No blocks yet.</p> : (
        <ol>
          {cycles.map((c) => (
            <li key={c.first} className={c.current ? 'current' : ''} onClick={() => nav(`/block/${c.first}`)} title="Open the cycle's first block">
              <div className="cyc-range">Blocks {c.start}–{c.end} <span className="muted">· {c.blocks}</span></div>
              <div>{c.stars} ★{c.keys ? ` · ${c.keys} 🔑` : ''} <span className="muted">· {fmtTime(c.frames)}</span></div>
              <div className="muted">{c.current && !c.ends ? 'in progress' : c.grand || c.ends ? '★ Grand Star: game beaten' : 'reset after a stall'}</div>
            </li>
          ))}
        </ol>
      )}
    </aside>
  );
}
