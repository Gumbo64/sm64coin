// The runs that lie on a block's envelope graph (the time-limit curve over the seconds since the
// parent, a dot per run at (its stamp - the parent's stamp, its frames)), and the replays each run
// offers. Pure data shaping, no React: Block.jsx fetches, this makes the chart's and picker's rows.
//
// Where runs come from:
//  - the winner: the block itself (header timestamp, sm64_frames).
//  - siblings: other blocks the node knows at the same height (alternate chains; get_alternate_chains
//    is not on restricted RPC, so the list is simply empty there).
//  - v0.4 included proofs: get_block will carry the other valid runs as `sm64_records`, an array
//    of {timestamp, frames, hash?, miner?, pads?}. Read here as long as it is that array; any
//    other shape yields no dots rather than a broken page. (Stamps are unix seconds; `t` seconds
//    after the parent is also accepted, for a node that does the subtraction.)
import { maxFrames } from './rules.js';

const num = (x) => (typeof x === 'number' && Number.isFinite(x) ? x : null);

/** The extra runs a block carries (v0.4 `sm64_records`), as {t, frames, hash?, miner?, pads?}. */
export function recordRuns(blk, parentTs) {
  const recs = Array.isArray(blk?.sm64_records) ? blk.sm64_records : [];
  const out = [];
  for (const r of recs) {
    if (!r || typeof r !== 'object') continue;
    const f = num(r.frames);
    const t = num(r.t) ?? (num(r.timestamp) !== null && parentTs ? r.timestamp - parentTs : null);
    if (f === null || t === null) continue;
    out.push({ t: Math.max(0, t), frames: f, hash: r.hash || null, miner: r.miner || null, pads: r.pads || null, source: 'record' });
  }
  return out;
}

/**
 * All the runs of `block`'s height: the winner first, then `alts` (get_block replies of sibling
 * blocks at the same height) and the winner's included records. Each {id, t, frames, winner,
 * source: 'chain' | 'alt' | 'record', hash?, miner?}. Runs on the same dot share (t, frames):
 * the chart groups them, the picker lists them all.
 */
export function runsAt(block, parentTs, alts = []) {
  const h = block.block_header;
  const runs = [{ id: `w:${h.hash}`, t: Math.max(0, h.timestamp - parentTs), frames: block.sm64_frames, winner: true, source: 'chain', hash: h.hash, miner: block.miner_tx_hash || null }];
  for (const a of alts) {
    const ah = a.block_header;
    if (!ah || ah.hash === h.hash || ah.height !== h.height || ah.prev_hash !== h.prev_hash) continue;
    runs.push({ id: `a:${ah.hash}`, t: Math.max(0, ah.timestamp - parentTs), frames: a.sm64_frames, winner: false, source: 'alt', hash: ah.hash, miner: a.miner_tx_hash || null });
  }
  recordRuns(block, parentTs).forEach((r, i) => runs.push({ ...r, id: `r:${i}`, winner: false }));
  return runs;
}

/** Runs sharing a dot (same second and frame count), the dot's list in the winner-first order. */
export function groupRuns(runs) {
  const m = new Map();
  for (const r of runs) {
    const k = `${Math.round(r.t)}:${r.frames}`;
    if (!m.has(k)) m.set(k, { t: r.t, frames: r.frames, runs: [], winner: false });
    const g = m.get(k);
    g.runs.push(r);
    g.winner ||= !!r.winner;
  }
  return [...m.values()];
}

/**
 * Seconds the limit curve is shifted right for a restart from power-on (judged against the limit
 * `restart_delay` seconds earlier): the elapsed time at which the curve gives the block's own
 * sm64_max_frames, subtracted from the block's real elapsed time. 0 when it does not apply.
 */
export function limitShift(difficulty, t, maxF) {
  if (!difficulty || !maxF || maxF <= 600) return 0;
  let lo = 0, hi = 1; // the limit grows with elapsed time: the smallest e reaching maxF
  while (maxFrames(difficulty, hi) < maxF && hi < 1e7) hi *= 2;
  while (hi - lo > 1) { const m = Math.floor((lo + hi) / 2); if (maxFrames(difficulty, m) >= maxF) hi = m; else lo = m; }
  const e = maxFrames(difficulty, lo) >= maxF ? lo : hi;
  return Math.max(0, Math.round(t - e));
}

export const TAIL_SECONDS = [5, 10, 30];

/** The frame (of the cycle, 0 = its first) to start the viewer at to watch the last `seconds` of a run that ends at `end` and lasts `runFrames`. */
export function tailFrame(end, runFrames, seconds) {
  return Math.max(end - runFrames, end - Math.round(seconds * 30));
}

/**
 * The replays one run offers, as rows the picker lists: {key, label, hint, to? | download? | disabled}.
 * `ctx`: {height, hash, cycleEnd (frames of the cycle up to the winner, or null while loading),
 * hasPads (the winner's played pads are loaded)}. Only the winner (a block of this chain) has a
 * replay here; a sibling opens its own block page; a record without pads has none yet.
 */
export function replayOptions(run, ctx) {
  const opts = [];
  if (run.source === 'chain' && run.winner) {
    const ready = ctx.cycleEnd != null;
    const off = ready ? undefined : 'loading the run…';
    const base = `/block/${run.hash}?replay`;
    opts.push({ key: 'start', label: 'Watch from its start state', hint: `${run.frames} frames`, to: base, disabled: off });
    for (const s of TAIL_SECONDS) {
      if (run.frames <= s * 30) continue;
      opts.push({ key: `tail${s}`, label: `Watch the last ${s} s`, to: ready ? `${base}&f=${tailFrame(ctx.cycleEnd, run.frames, s)}` : base, disabled: off });
    }
    opts.push({ key: 'm64', label: 'Download .m64 (this run)', download: 'run', disabled: ctx.hasPads ? undefined : 'loading the run…' });
    opts.push({ key: 'cycle', label: 'Download .m64 (whole cycle)', download: 'cycle', disabled: ready ? undefined : 'loading the run…' });
  } else if (run.source === 'alt') {
    opts.push({ key: 'open', label: "Open this block's page", hint: 'a sibling block the node saw at this height', to: `/block/${run.hash}` });
  } else {
    opts.push({ key: 'none', label: 'Replay not available yet', hint: run.pads ? 'its inputs are included' : 'only its stamp and length are known', disabled: run.pads ? 'not wired yet' : 'no inputs' });
  }
  return opts;
}
