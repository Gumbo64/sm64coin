// The "Places reached" chart's data: how many distinct floors the searches reached in a step (the reel's new-place floors:
// level, area, floor normal, plane offset, moving object; coplanar pieces are one place), over search time.
// Each worker's viz line carries `cov` = {places (its tree's count), records (broken so far), new: [hashed floor keys]}.
//
// Independent workers are rarely on the same step: a step's key holds the worker's own seed, and the workers can be on
// different courses. Floors are the same floors whoever reaches them, so workers are compared by the step's place
// (level, area, goal: the group key) and each worker counts only with its current step's floors. The chart follows
// the place most workers are on (ties: the furthest one); union / per worker / overlap are over the workers there,
// and every sample says how many are ("2 of 3 workers on Whomp's Fortress"). A change of place starts a new run of
// the lines (marked), so a flattening curve and a fresh step are told apart.

import { LEVELS, levelName } from './levels.js';

const BUCKET_S = 15; // the status strip's bucket
const EXPLORING_MIN = 3; // new floors in a bucket for "new places"
const MAX_SAMPLES = 4000;
const MAX_STEPS = 32; // per-worker step sets kept (the current ones are never dropped)

/** The place a step key (`seed:frame0:level:area:goal`, vizFeed.js) is about: level, area and goal, without the worker's seed and frame. */
export function groupOf(key) {
  const p = String(key).split(':');
  return p.length >= 5 ? `${p[2]}:${p[3]}:${p.slice(4).join(':')}` : String(key);
}
const placeName = (group) => levelName(+group.split(':')[0]);
const placeIcon = (group) => (LEVELS[+group.split(':')[0]] || [])[1] || placeName(group).slice(0, 4);

export class Coverage {
  constructor() {
    this.t0 = 0; // Date.now() (ms) of the first sample of the round
    this.steps = new Map(); // step key -> {w, group, set: Set of floor keys, places, records}
    this.cur = []; // per worker: its current step key
    this.lead = null; // the group the chart follows
    this.series = []; // {t, key (group), union, per: [places per worker, null when not there], rec, n (workers there), of, label}
    this.markers = []; // {t, label, icon}: where the chart's place changed
    this.runs = []; // {t}: seconds (chart time) when a worker found a run; the workers then restart to beat it
    this.lastPush = 0;
  }

  /** A viz line of worker `i` (`cov`, step key `key`); `n`: workers, `stepNo(k)`: worker k's step number (for ties). */
  add(i, key, cov, n, stepNo = () => 0) {
    if (!cov) return;
    const now = Date.now();
    if (!this.t0) this.t0 = now;
    let st = this.steps.get(key);
    if (!st) {
      st = { w: i, group: groupOf(key), set: new Set(), places: 0, records: 0 };
      this.steps.set(key, st);
      const live = new Set(this.cur);
      live.add(key);
      for (const k of this.steps.keys()) {
        if (this.steps.size <= MAX_STEPS) break;
        if (!live.has(k)) this.steps.delete(k);
      }
    }
    for (const k of cov.new || []) st.set.add(k);
    st.places = cov.places || 0;
    st.records = cov.records || 0;
    this.cur[i] = key;
    if (now - this.lastPush < 900) return;
    // The place most workers are on; ties: the one with the furthest worker.
    const by = new Map();
    for (let k = 0; k < n; k++) {
      const s = this.steps.get(this.cur[k]);
      if (!s) continue;
      const g = by.get(s.group) || { n: 0, far: -1 };
      g.n++;
      g.far = Math.max(g.far, stepNo(k) || 0);
      by.set(s.group, g);
    }
    let lead = null, lg = null;
    for (const [g, v] of by) if (!lg || v.n > lg.n || (v.n === lg.n && v.far > lg.far)) { lead = g; lg = v; }
    if (lead === null) return;
    this.lastPush = now;
    const t = (now - this.t0) / 1000;
    const label = placeName(lead);
    if (this.lead !== lead) {
      if (this.lead !== null) this.markers.push({ t, label, icon: placeIcon(lead) });
      this.lead = lead;
    }
    const union = new Set();
    const per = new Array(n).fill(null);
    let rec = 0;
    for (let k = 0; k < n; k++) {
      const s = this.steps.get(this.cur[k]);
      if (!s || s.group !== lead) continue;
      for (const f of s.set) union.add(f);
      per[k] = s.places;
      rec += s.records;
    }
    this.series.push({ t, key: lead, union: union.size, per, rec, n: lg.n, of: n, label });
    if (this.series.length > MAX_SAMPLES) this.series = this.series.filter((_, k) => k % 2 === 0 || k === this.series.length - 1);
  }

  /** A worker found a run now: marks it on the chart (from here the workers search for a shorter one). */
  runFound() {
    const t = this.t0 ? (Date.now() - this.t0) / 1000 : 0;
    this.runs.push({ t });
  }

  /** The latest overlap = union / sum of the counts of the workers on the chart's place (1: each place found by one worker;
   *  low: duplicated work), or null with fewer than two workers there. */
  overlap() {
    const s = this.series[this.series.length - 1];
    if (!s || s.n < 2) return null;
    const sum = s.per.reduce((a, b) => a + (b || 0), 0);
    return sum ? Math.min(1, s.union / sum) : null;
  }

  /** Buckets of BUCKET_S seconds: {t0, kind: 'new' (new places) | 'faster' (faster routes to known spots) | 'noprogress' |
   *  'none' (no data: no worker sample arrived in the slice), fresh, records, beating (after a run was found), run (a run was found in it)}. */
  buckets() {
    const out = [];
    let prev = null;
    for (const s of this.series) {
      const b = Math.floor(s.t / BUCKET_S);
      while (out.length <= b) out.push({ t0: out.length * BUCKET_S, fresh: 0, records: 0, n: 0 });
      const o = out[b];
      o.n++;
      if (prev) {
        o.fresh += prev.key === s.key ? Math.max(0, s.union - prev.union) : s.union;
        o.records += prev.key === s.key ? Math.max(0, s.rec - prev.rec) : 0;
      }
      prev = s;
    }
    const first = this.runs.length ? this.runs[0].t : Infinity;
    return out.map((o) => ({
      ...o,
      kind: !o.n ? 'none' : o.fresh >= EXPLORING_MIN ? 'new' : o.records >= 1 ? 'faster' : 'noprogress',
      beating: o.t0 + BUCKET_S > first,
      run: this.runs.some((r) => Math.floor(r.t / BUCKET_S) * BUCKET_S === o.t0),
    }));
  }
}

export const BUCKET_COLORS = { new: '#4ade80', faster: '#60a5fa', noprogress: '#f87171', none: '#2a2f4a' };
export const BUCKET = BUCKET_S;
