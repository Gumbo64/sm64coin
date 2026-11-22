import React, { useEffect, useMemo, useState } from 'react';
import { Link, useNavigate } from 'react-router';
import { Viewer } from 'sm64-replay-ui';
import { CyclePlayer } from '../lib/cyclePlayer.js';
import { eventName } from '../lib/CycleTimeline.jsx';
import { daemon } from '../lib/rpc.js';
import { rawToPads } from '../lib/proof.js';
import { withConfig } from '../lib/task.js';
import { successFor } from '../lib/rules.js';
import { frames } from '../lib/format.js';

const COLORS = ['#f7c948', '#60a5fa', '#4ade80', '#f472b6', '#fb923c', '#a78bfa', '#2dd4bf', '#f87171'];

// A block that starts from power-on starts a new run: what is before it in
// the answer is another run.
function activeRun(cycle) {
  let k = 0;
  cycle.blocks.forEach((b, i) => { if (b.power_on) k = i; });
  if (k === 0) return cycle;
  const skipped = cycle.blocks.slice(0, k).reduce((s, x) => s + x.frames, 0);
  return { ...cycle, blocks: cycle.blocks.slice(k), played_pads: cycle.played_pads.slice(skipped * 8) };
}

/**
 * The block page's replay: sm64-viewer's whole viewer (shared, see
 * vite.config.js) playing the cycle up to and including this block, the
 * block's frames re-checked against the perturbation. The block's details
 * (`panel`) take the place of the viewer's regions list.
 */
export default function BlockReplay({ block, cycle, panel }) {
  const nav = useNavigate();
  const [won, setWon] = useState(null);
  const [err, setErr] = useState(null);
  useEffect(() => {
    daemon('get_info').then((info) => {
      const w = successFor(info.sm64_rules);
      if (w) setWon(() => w);
      else setErr(`this page doesn't know the success condition of rules "${info.sm64_rules}" (see web/src/lib/rules.js)`);
    }).catch((e) => setErr(String(e)));
  }, []);

  const h = block.block_header;
  const run = useMemo(() => {
    const c = activeRun(cycle);
    let at = 0;
    const regions = c.blocks.map((b, i) => {
      const r = { from: at, to: at + b.frames, label: eventName(b.event), color: COLORS[i % COLORS.length],
        note: `block ${b.height} · ${frames(b.frames)}${b.power_on ? ' · from power-on' : ''}`, hash: b.hash };
      at += b.frames;
      return r;
    });
    const start = regions[regions.length - 1].from;
    const milestones = [...new Set([0, ...regions.flatMap((r) => [r.from, r.to])])].sort((a, b) => a - b);
    return { pads: rawToPads(c.played_pads), regions, start, milestones, last: c.blocks[c.blocks.length - 1] };
  }, [cycle]);

  if (err) return <div className="card error">{err}</div>;
  if (!won) return <div className="card">Loading…</div>;
  const task = withConfig(block);
  const replay = { name: `Block ${h.height}: ${eventName(run.last.event)}`, pads: run.pads, regions: run.regions };
  return (
    <div className="replay-host">
      <Viewer
        replay={replay}
        initial={{ frame: run.start, speed: 1, playing: true }}
        onRewind={() => {}}
        onClose={() => nav('/')}
        backLabel="← Explorer"
        milestones={run.milestones}
        /* window.__player: for the e2e scripts */
        makePlayer={(r, onChange) => (window.__player = new CyclePlayer(r.pads, onChange, { start: run.start, task, won }))}
        topbarExtra={() => (
          <span className="row tight small">
            <Link to={`/block/${h.height - 1}`} title="Previous block">‹ block {h.height - 1}</Link>
            <Link to={`/block/${h.height + 1}`} title="Next block">block {h.height + 1} ›</Link>
          </span>
        )}
        regionLink={(r) => r.hash && <Link to={`/block/${r.hash}`} title="Open this block's page">open ↗</Link>}
        panelExtra={panel}
      />
    </div>
  );
}
