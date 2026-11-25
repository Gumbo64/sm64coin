// The visualisations' data hub: everything the Auto-mine page's views draw, fed by the page from the
// workers' messages (see lib/webminer/vizFeed.js). No React, no three.js, no game engine here: this is
// the only part that stays loaded when visualisations are off, and then every method returns at once.
// Views read the public fields directly and are woken at most 4 times a second (`subscribe`).
import { parseViz, parseLog, flatToRaw, proofInputs } from '../../lib/webminer/vizFeed.js';

/** The point cloud keeps the newest CAP sampled search nodes (a ring buffer). */
export const CAP = 50000;
const ENV_CAP = 4000;

export class VizHub {
  constructor() {
    this.enabled = false; // collecting (the visualisations are switched on)
    this.live = false; // the Auto-mine page is on screen: views are woken
    this.subs = new Set();
    this.pending = null;
    this.version = 0;
    this.blocks = []; // our submissions this session
    this.toasts = [];
    this.round = 0;
    this.clear();
  }

  clear() {
    this.template = null;
    this.startKind = 'parent';
    this.parentRaw = null; // the parent's end state (the node's SaveState bytes), when the node had it
    this.workers = [];
    this.runs = []; // finished runs: {t (s after the parent), frames, w}
    this.samples = []; // per-sample best: {w, frame (from the block's start), p (stars + keys), t}
    this.found = null; // the run being submitted: {w, frames, inputs, power, template, status, height}
    this.game = { id: 0 }; // what the game view plays: {id, kind: 'best'|'run', w}
    this.selected = null; // {w, id}
    // Point cloud ring buffer.
    this.n = 0; // nodes ever added
    this.pos = new Float32Array(CAP * 3);
    this.frame = new Float32Array(CAP);
    this.stars = new Uint8Array(CAP);
    this.worker = new Uint8Array(CAP);
    this.level = new Uint8Array(CAP);
    this.area = new Uint8Array(CAP);
    this.flags = new Uint8Array(CAP);
    this.dist = new Float32Array(CAP).fill(-1); // frames to go to the step's target (negative: none known)
    this.nodeId = new Int32Array(CAP);
    this.stepOf = new Uint16Array(CAP); // the worker's search-step number (w.cur.n) a node belongs to
    this.paths = new Map(); // `${worker}:${step key}:${node id}` -> raw pads (4 bytes a frame) of a node's path from its step's root, or 'asked'
    this.parentSlot = new Int32Array(CAP).fill(-1);
    this.keyAt = new Float64Array(CAP).fill(-1);
    this.index = new Map(); // (worker, step, id) -> slot
    this.bumpCloud();
    this.cloudChanged = true;
  }

  bumpCloud() { this.cloudVersion = (this.cloudVersion || 0) + 1; }

  subscribe(fn) { this.subs.add(fn); return () => this.subs.delete(fn); }

  /** Wakes the views, at most every 250 ms. */
  notify() {
    if (!this.enabled || !this.live || this.pending) return;
    this.pending = setTimeout(() => {
      this.pending = null;
      this.version++;
      this.subs.forEach((f) => f(this.version));
    }, 250);
  }

  setEnabled(on) {
    this.enabled = !!on;
    if (!on) { this.clear(); this.workers = []; this.blocks = []; }
    this.notify();
    if (!on) { this.version++; this.subs.forEach((f) => f(this.version)); }
  }

  /** A round starts: a new template, `count` workers each on `starts[i]`. */
  newRound({ template, starts, parentRaw }) {
    if (!this.enabled) return;
    const keep = { blocks: this.blocks, toasts: this.toasts };
    this.clear();
    this.blocks = keep.blocks;
    this.toasts = keep.toasts;
    this.round++;
    this.template = template;
    this.startKind = starts[0] || 'parent';
    this.parentRaw = parentRaw || null;
    this.portable = {};
    this.t0 = Date.now() / 1000;
    this.workers = starts.map((start, i) => ({
      i, start, line: 'loading the game…', steps: [], cur: null, stepNo: 0, played: 0, keysBase: 0, starsBase: 0,
      fps: 0, lastSim: null, nodesTotal: 0, best: null, bestPads: null, bestStep: null, prefix: null, root: null, keys: {}, found: false, runs: 0,
    }));
    this.notify();
  }

  /** Worker `i` begins a new search (a fresh campaign) from `start`. */
  restartWorker(i, start) {
    const w = this.workers[i];
    if (!w) return;
    Object.assign(w, { start, line: 'loading the game…', steps: [], cur: null, stepNo: 0, played: 0, keysBase: 0, starsBase: 0, fps: 0, lastSim: null, nodesTotal: 0, best: null, bestPads: null, bestStep: null, prefix: null, root: null, keys: {} });
    this.notify();
  }

  /** A campaign log line of worker `i`. */
  log(i, line) {
    if (!this.enabled) return;
    const w = this.workers[i];
    if (!w) return;
    w.line = line;
    const ev = parseLog(line);
    if (ev.kind === 'step') {
      w.stepNo = ev.n;
      w.played = ev.frame;
      if (ev.stars >= 0) { w.starsBase = ev.stars; w.keysBase = ev.keys; }
      w.steps.push({ n: ev.n, name: ev.name, frame: ev.frame, budget: ev.budget, stars: ev.stars, keys: ev.keys, done: null, t0: Date.now() });
      if (w.steps.length > 60) w.steps.shift();
      w.cur = null;
    } else if (ev.kind === 'done') {
      const s = w.steps[w.steps.length - 1];
      if (s) s.done = ev;
    }
    this.notify();
  }

  /** A viz line of worker `i` (about once a second per running search tree). */
  ingest(i, line) {
    if (!this.enabled) return;
    const w = this.workers[i];
    const m = parseViz(line);
    if (!w || !m) return;
    const now = performance.now();
    if (!w.cur || w.cur.key !== m.key) {
      w.curN = (w.curN || 0) + 1;
      w.cur = { key: m.key, n: w.curN, step: m.step, target: m.step.target, t: 0, nodes: 0, sim: 0, best: null };
      w.keys[w.curN & 4095] = m.key;
      w.lastSim = null;
      w.best = null;
      w.bestPads = null;
      w.prefix = m.prefix || null;
      // The step's root is the cloud's first point.
      this.add(i, w.cur.n, 0, -1, w.played, m.step.pos, w.starsBase, m.step.level, m.step.area, 0, m.step.target ? m.step.target.d0 : -1);
    }
    if (m.prefix) w.prefix = m.prefix;
    const c = w.cur;
    c.t = m.t; c.nodes = m.nodesTotal; c.sim = m.simFrames;
    w.nodesTotal = m.nodesTotal;
    if (w.lastSim && m.simFrames >= w.lastSim.sim && now > w.lastSim.at) w.fps = (m.simFrames - w.lastSim.sim) / ((now - w.lastSim.at) / 1000);
    w.lastSim = { sim: m.simFrames, at: now };
    for (const n of m.nodes) {
      this.add(i, c.n, n[0], n[1], w.played + n[2], [n[3], n[4], n[5]], n[6], n[7], n[8], n[9], n.length > 10 ? n[10] : -1);
    }
    if (m.best) {
      const prev = w.best;
      w.best = { ...m.best, frameAbs: w.played + m.best.frame };
      c.best = w.best;
      if (m.pads) { w.bestPads = m.pads; w.bestStep = { played: w.played, key: m.key, prefix: w.prefix }; }
      const p = m.best.stars + m.best.keys;
      if (!prev || prev.stars + prev.keys !== p || prev.frame !== m.best.frame) {
        this.samples.push({ w: i, frame: w.played + m.best.frame, p, t: Date.now() / 1000 });
        if (this.samples.length > ENV_CAP) this.samples.splice(0, this.samples.length - ENV_CAP);
      }
      if (m.pads) {
        // The best improved: the game view replays it (unless a found run is on).
        if (!this.found) this.game = { id: this.game.id + 1, kind: 'best', w: i };
        if (!prev || p > prev.stars + prev.keys) this.toast(`worker ${i + 1}: ${m.best.stars} ★${m.best.keys ? ` ${m.best.keys} 🔑` : ''} reached at frame ${w.played + m.best.frame}`, 'best');
      }
    }
    this.notify();
  }

  add(w, step, id, parent, frame, pos, stars, level, area, flags, dist = -1) {
    const slot = this.n % CAP;
    const old = this.keyAt[slot];
    if (old >= 0) this.index.delete(old);
    const key = (w * 4096 + (step & 4095)) * 1e8 + id;
    this.keyAt[slot] = key;
    this.index.set(key, slot);
    this.pos[slot * 3] = pos[0]; this.pos[slot * 3 + 1] = pos[1]; this.pos[slot * 3 + 2] = pos[2];
    this.frame[slot] = frame;
    this.stars[slot] = stars;
    this.worker[slot] = w;
    this.level[slot] = level;
    this.area[slot] = area;
    this.flags[slot] = flags;
    this.dist[slot] = dist;
    this.nodeId[slot] = id;
    this.stepOf[slot] = step & 4095;
    this.parentSlot[slot] = parent >= 0 ? this.index.get((w * 4096 + (step & 4095)) * 1e8 + parent) ?? -1 : -1;
    this.n++;
    this.cloudChanged = true;
  }

  /** The ancestors of `slot` that are in the buffer (sampling thins the tree), nearest first. */
  chain(slot) {
    const out = [];
    for (let s = this.parentSlot[slot], guard = 0; s >= 0 && guard < 2000; s = this.parentSlot[s], guard++) out.push(s);
    return out;
  }

  /** The slots to draw: the newest min(n, CAP), in ring order. */
  count() { return Math.min(this.n, CAP); }

  /** A worker's run was found: it is what the game view plays now. */
  foundRun(i, r, template) {
    if (!this.enabled) return;
    let inputs = null, power = false;
    try { ({ pads: inputs, power } = proofInputs(r.proof)); } catch { /* unreadable: no replay */ }
    this.runs.push({ t: Date.now() / 1000 - (template.sm64_parent_timestamp || 0), frames: r.frames, w: i });
    this.found = { w: i, frames: r.frames, inputs, power, template, start: r.start, status: 'found', height: template.height, id: ++this.game.id };
    this.game = { id: this.game.id, kind: 'run', w: i };
    this.toast(`worker ${i + 1} found a run: ${r.frames} frames`, 'found');
    this.notify();
  }

  /** The submission's outcome: 'sent' | 'accepted' | 'rejected' | 'stale' | 'held'. */
  submission(status, info = {}) {
    if (!this.enabled) return;
    const f = this.found;
    if (f) f.status = status;
    if (['accepted', 'rejected', 'stale'].includes(status)) {
      this.blocks.unshift({ at: Date.now(), status, height: info.height ?? f?.height, frames: info.frames ?? f?.frames, w: f?.w, note: info.note || '' });
      if (this.blocks.length > 50) this.blocks.pop();
    }
    this.notify();
  }

  toast(text, kind = 'info') {
    const t = { id: Math.random(), text, kind, at: Date.now() };
    this.toasts = [...this.toasts.filter((x) => Date.now() - x.at < 7000), t].slice(-4);
  }

  select(sel) { this.selected = sel; this.notify(); }

  /** A worker's step start state arrived (SaveState bytes of the headless game); only the newest step's is kept. */
  setRoot(i, key, raw) {
    const w = this.workers[i];
    if (!w || !this.enabled) return;
    w.root = { key, raw };
    this.notify();
  }

  /** The inputs of node `id` (asked for with requestPath) arrived: flat [button, x, y, ...] from its step's root. */
  setPath(i, key, id, flat) {
    if (!this.enabled) return;
    this.paths.set(`${i}:${key}:${id}`, flatToRaw(flat));
    this.notify();
  }

  /**
   * What the game view needs to play a node of worker `w`'s current step: {raw (pads from the step's root), rootRaw
   * (the step's start state, or null: the block's start when the step starts at frame 0), key, played, best} or {wait: why}
   * (the node's inputs are being asked of the worker) or {error: why}.
   */
  nodePlay(w, stepN, nid) {
    const wk = this.workers[w];
    if (!wk || !wk.cur) return { wait: 'waiting for the search…' };
    if ((stepN & 4095) !== (wk.cur.n & 4095)) return { error: 'that node belongs to a search step that is over: its start state is gone. Pick a node of the current step (or play the best path).' };
    const key = wk.cur.key;
    const base = { key, played: wk.played, best: wk.best, rootRaw: wk.root && wk.root.key === key ? wk.root.raw : null };
    if (wk.best && wk.best.id === nid && wk.bestPads) return { ...base, raw: flatToRaw(wk.bestPads) };
    const pk = `${w}:${key}:${nid}`;
    const got = this.paths.get(pk);
    if (got && got !== 'asked') return { ...base, raw: got };
    if (!got) {
      if (!this.requester || !this.requester(w, key, nid)) return { error: 'only a search\'s best node has its inputs here; switch on worker sync (the button above Start) to play any node' };
      this.paths.set(pk, 'asked');
    }
    return { wait: `asking worker ${w + 1} for the node's inputs…` };
  }

  /** Pads of worker `i`'s best path. */
  bestPlay(i) {
    const w = this.workers[i];
    if (!w || !w.bestPads || !w.cur) return null;
    const p = this.nodePlay(i, w.cur.n, w.best.id);
    return p.raw ? p : null;
  }
}
