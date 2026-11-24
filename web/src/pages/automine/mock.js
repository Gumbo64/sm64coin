// A development feed for the visualisations: fake workers emitting what the real ones do (lib/webminer/vizFeed.js
// documents the format), so the views can be built and tested without a running search. Used with
// /automine?vizmock=1. The positions are random walks (the game view replays the pads on the real start state,
// so its end position does not match: the view says "simulated").
import { proofFromRun, bytesToHex } from '../../lib/proof.js';

const ZONES = [[16, 1, 'Castle Grounds'], [6, 1, 'Castle'], [9, 1, 'Bob'], [6, 1, 'Castle'], [12, 1, 'Jrb'], [24, 1, 'Wf']];

function rng(seed) { let s = seed >>> 0 || 1; return () => { s ^= s << 13; s >>>= 0; s ^= s >>> 17; s ^= s << 5; s >>>= 0; return s / 4294967296; }; }

export function startMock(hub, count, { runAfter = 0 } = {}) {
  hub.mock = true;
  const timers = [];
  for (let i = 0; i < count; i++) {
    const r = rng(1234 + i * 77);
    let step = 0, zi = 0, nodes = [], frame0 = 0, t = 0, bestId = 0, padsAll = [], sim = 0, stars = 0, started = false;
    const begin = () => {
      const [level, area, name] = ZONES[zi % ZONES.length];
      step++;
      nodes = [{ id: 0, parent: -1, frame: 0, p: [(r() - 0.5) * 800, 200, (r() - 0.5) * 800] }];
      bestId = 0; padsAll = []; t = 0; sim = 0;
      hub.log(i, `step ${step}: ${name} at frame ${frame0}, ${stars} stars 0 keys: mock; budget ${200 + zi * 50} M frames`);
      return { level, area };
    };
    let z = begin();
    const tick = () => {
      if (!hub.enabled || !hub.workers[i]) return;
      if (!started) started = true;
      t += 1;
      const fresh = [];
      const N = 300 + Math.floor(r() * 300);
      for (let k = 0; k < N; k++) {
        const par = nodes[Math.floor(r() * nodes.length * (0.6 + 0.4 * r()))] || nodes[0];
        const fr = par.frame + 1 + Math.floor(r() * 25);
        const ang = r() * 6.283, d = 20 + r() * 140;
        const nd = { id: nodes.length, parent: par.id, frame: fr, p: [par.p[0] + Math.cos(ang) * d, par.p[1] + (r() - 0.45) * 12, par.p[2] + Math.sin(ang) * d] };
        nodes.push(nd);
        fresh.push(nd);
        if (nd.frame > nodes[bestId].frame) bestId = nd.id;
      }
      sim += N * 900;
      const gotStar = r() < 0.04 && t > 3;
      const b = nodes[bestId];
      const sNew = gotStar ? stars + 1 : stars;
      const line = JSON.stringify({
        step: { frame0: 100000 + frame0, seed: 7, goal: 'Star', level: z.level, area: z.area, pos: nodes[0].p },
        t, nodes_total: nodes.length, sim_frames: sim,
        best: { id: b.id, frame: b.frame, stars: sNew, keys: 0, pos: b.p },
        nodes: fresh.map((n) => [n.id, n.parent, n.frame, n.p[0], n.p[1], n.p[2], sNew, z.level, z.area, n.frame > 200 ? 1 : 0]),
        pads: (() => { padsAll = []; for (let f = 0; f < b.frame; f++) padsAll.push(f % 90 < 60 ? 0 : 0x8000, Math.round(Math.sin(f / 40 + i) * 60), 100); return padsAll; })(),
      });
      hub.ingest(i, line);
      if (gotStar) stars = sNew;
      if (t > 25 + i * 3) {
        hub.log(i, `  ${ZONES[zi % ZONES.length][2]} at frame ${frame0 + b.frame} (+${b.frame}), ${sim / 1e6} M frames, ${nodes.length} nodes (${t} s)`);
        frame0 += b.frame; zi++; z = begin();
      }
    };
    timers.push(setInterval(tick, 1000));
  }
  if (runAfter) {
    timers.push(setTimeout(() => {
      const frames = 420;
      const pads = Array.from({ length: frames }, (_, f) => ({ button: f % 90 < 60 ? 0 : 0x8000, stick_x: Math.round(Math.sin(f / 40) * 60), stick_y: 100 }));
      hub.foundRun(0, { frames, proof: bytesToHex(proofFromRun(pads, pads.map(() => false), false)), start: 'parent' }, hub.template);
      timers.push(setTimeout(() => hub.submission('accepted', { height: hub.template.height }), 15000));
    }, runAfter));
  }
  return () => { timers.forEach((x) => { clearInterval(x); clearTimeout(x); }); hub.mock = false; };
}

/** A round on the chain's real tip without a miner address: a template made from get_info (no block is ever sealed). */
export async function mockRound(hub, count) {
  const { daemon } = await import('../../lib/rpc.js');
  const { hexToBytes } = await import('../../lib/proof.js');
  const info = await daemon('get_info');
  const hdr = await daemon('get_block_header_by_hash', { hash: info.top_block_hash });
  let parentRaw = null;
  try {
    const s = await daemon('get_sm64_state', { hash: info.top_block_hash });
    parentRaw = await new Response(new Blob([hexToBytes(s.state_gz)]).stream().pipeThrough(new DecompressionStream('gzip'))).arrayBuffer();
  } catch { /* an older node: the game view then has no parent state */ }
  const t = {
    ...info,
    height: info.height,
    prev_hash: info.top_block_hash,
    sm64_seed_hash: '00'.repeat(32),
    sm64_retarget_difficulty: info.difficulty,
    sm64_parent_timestamp: hdr.block_header.timestamp,
    sm64_start: 'parent',
    sm64_power_on_at: 0,
    sm64_parent_stars: 0,
    sm64_parent_keys: 0,
  };
  hub.newRound({ template: t, starts: Array.from({ length: count }, () => 'parent'), parentRaw });
  return t;
}
