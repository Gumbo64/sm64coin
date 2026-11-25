// The "Places reached" chart's data: how many distinct floors the searches reached in a step (the reel's new-place floors:
// level, area, floor normal, plane offset, moving object; coplanar pieces are one place), over search time.
// Each worker's viz line carries `cov` = {places (its tree's count), records (broken so far), new: [hashed floor keys]};
// the page keeps the union over workers per step. One timeline per round: x = seconds since the round's first sample;
// a step change starts a new run of the lines (marked), so a flattening curve and a fresh step are told apart.

const BUCKET_S = 15; // the status strip's bucket
const EXPLORING_MIN = 3; // new floors in a bucket for "exploring"
const MAX_SAMPLES = 4000;

export class Coverage {
  constructor() {
    this.t0 = 0; // Date.now() (ms) of the first sample of the round
    this.steps = new Map(); // step key -> {set: Set of floor keys, per: [places per worker], rec: [records per worker]}
    this.lead = null; // the step key the furthest worker is in
    this.series = []; // {t, key, union, per: [..], rec}
    this.markers = []; // {t, label}
    this.lastPush = 0;
  }

  /** A viz line of worker `i` (`cov`, step key `key`); `lead`: the key of the furthest worker's step, `n`: workers, `stepNo`: that step's number. */
  add(i, key, cov, lead, n, stepNo) {
    if (!cov) return;
    const now = Date.now();
    if (!this.t0) this.t0 = now;
    let st = this.steps.get(key);
    if (!st) {
      st = { set: new Set(), per: new Array(n).fill(0), rec: new Array(n).fill(0) };
      this.steps.set(key, st);
      while (this.steps.size > 6) this.steps.delete(this.steps.keys().next().value);
    }
    for (const k of cov.new || []) st.set.add(k);
    st.per[i] = cov.places || 0;
    st.rec[i] = cov.records || 0;
    if (key !== lead) return;
    const t = (now - this.t0) / 1000;
    if (this.lead !== key) {
      if (this.lead !== null) this.markers.push({ t, label: `step ${stepNo}` });
      this.lead = key;
    }
    if (now - this.lastPush < 900) return;
    this.lastPush = now;
    this.series.push({ t, key, union: st.set.size, per: st.per.slice(), rec: st.rec.reduce((a, b) => a + b, 0) });
    if (this.series.length > MAX_SAMPLES) this.series = this.series.filter((_, k) => k % 2 === 0 || k === this.series.length - 1);
  }

  /** The latest overlap = union / sum of the workers' own counts (1: each place found by one worker; low: duplicated work), or null. */
  overlap() {
    const s = this.series[this.series.length - 1];
    if (!s) return null;
    const sum = s.per.reduce((a, b) => a + b, 0);
    return sum ? s.union / sum : null;
  }

  /** Buckets of BUCKET_S seconds: {t0, kind: 'exploring' | 'refining' | 'stuck' | 'none', fresh, records}. */
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
    return out.map((o) => ({ ...o, kind: !o.n ? 'none' : o.fresh >= EXPLORING_MIN ? 'exploring' : o.records >= 1 ? 'refining' : 'stuck' }));
  }
}

export const BUCKET_COLORS = { exploring: '#4ade80', refining: '#60a5fa', stuck: '#f87171', none: '#2a2f4a' };
export const BUCKET = BUCKET_S;
