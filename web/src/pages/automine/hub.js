// The visualisations' data hub: everything the Auto-mine page's views draw, fed by the page from the
// workers' messages (see lib/webminer/vizFeed.js). No React, no three.js, no game engine here: this is
// the only part that stays loaded when visualisations are off, and then every method returns at once.
// Views read the public fields directly and are woken at most 4 times a second (`subscribe`).
import { parseViz, parseLog, flatToRaw, proofInputs } from '../../lib/webminer/vizFeed.js';
import { TreeMirror } from '../../lib/webminer/treeMirror.js';
import { levelName } from './levels.js';
import { Coverage } from './coverage.js';

/** The point cloud keeps the newest CAP sampled search nodes (a ring buffer). */
export const CAP = 50000;
// The highlight reel's queue (clips waiting for the one playing to finish), and how old a clip may be when its turn comes.
const REEL_QUEUE = 8;
const REEL_STALE_MS = 120000;
// A node's inputs asked of a worker (nodePlay) and not answered by then are asked again.
const ASK_AGAIN_MS = 5000;
// The live game's settings (remembered in localStorage): where a replay starts playing at the game's pace ('end': the
// last `leadSec` seconds, the prefix before runs muted at full speed; 'step': from the step's start), the playback speed,
// which highlights the reel plays and the smallest record (frames saved) it plays (the worker sends records of 10+).
export const SETTINGS_KEY = 'sm64coin.gameSettings';
export const SPEEDS = [0.25, 0.5, 1, 2, 4];
const DEFAULT_SETTINGS = { open: false, start: 'end', leadSec: 5, speed: 1, places: true, records: true, minGain: 10 };
const loadSettings = () => { try { return { ...DEFAULT_SETTINGS, ...(JSON.parse(localStorage.getItem(SETTINGS_KEY) || 'null') || {}) }; } catch { return { ...DEFAULT_SETTINGS }; } };

/** A worker's campaign position, from its log lines (what the views need of it before any viz line). */
const liteWorker = (start) => ({ start, line: 'loading the game…', steps: [], stepNo: 0, played: 0, keysBase: 0, starsBase: 0 });

function logInto(w, line, ev) {
  w.line = line;
  if (ev.kind === 'step') {
    w.stepNo = ev.n;
    w.played = ev.frame;
    if (ev.stars >= 0) { w.starsBase = ev.stars; w.keysBase = ev.keys; }
    w.steps.push({ n: ev.n, name: ev.name, frame: ev.frame, budget: ev.budget, stars: ev.stars, keys: ev.keys, done: null, t0: Date.now() });
    if (w.steps.length > 60) w.steps.shift();
  } else if (ev.kind === 'done') {
    const s = w.steps[w.steps.length - 1];
    if (s) s.done = ev;
  }
}

export class VizHub {
  constructor() {
    this.goal = null; // the position goal {level, area, pos} (hybrid mining), kept across rounds
    this.enabled = false; // collecting (the visualisations are switched on)
    this.live = false; // the Auto-mine page is on screen: views are woken
    this.subs = new Set();
    this.pending = null;
    this.version = 0;
    this.blocks = []; // our submissions this session
    this.toasts = [];
    this.round = 0;
    this.settings = loadSettings();
    this.reel = { on: true, follow: null, queue: [], clips: 0 }; // the highlight reel: playing on/paused, follow worker N (null: all), queued clips
    // The current round's template and workers, kept (small: no tree data) while the visualisations are off, so that
    // switching them on mid-round shows the round's workers at once: {template, parentRaw, workers: [{start, stepNo,
    // played, starsBase, keysBase, line, steps}]}.
    this.roster = null;
    this.clear();
  }

  clear() {
    this.template = null;
    this.startKind = 'parent';
    this.parentRaw = null; // the parent's end state (the node's SaveState bytes), when the node had it
    this.workers = [];
    this.runs = []; // finished runs: {t (s after the parent), frames, w}
    this.cover = new Coverage(); // the "Places reached" chart's data (coverage.js)
    this.reel.queue = [];
    this.edgeBytes = 0;
    this.hl = null; // the node the game view highlights in the cloud: {w, step, id}
    this.found = null; // the run being submitted: {w, frames, inputs, power, template, status, height}
    this.game = { id: 0 }; // what the game view plays: {id, kind: 'best'|'run', w}
    this.selected = null; // {w, id}
    this.manual = null; // the last human-played segment: see playedSegment()
    // Point cloud ring buffer.
    this.n = 0; // nodes ever added
    this.pos = new Float32Array(CAP * 3);
    this.frame = new Float32Array(CAP);
    this.stars = new Uint8Array(CAP);
    this.worker = new Uint8Array(CAP);
    this.level = new Uint8Array(CAP);
    this.area = new Uint8Array(CAP);
    this.flags = new Uint8Array(CAP);
    this.nodeId = new Int32Array(CAP);
    this.stepOf = new Uint16Array(CAP); // the worker's search-step number (w.cur.n) a node belongs to
    this.paths = new Map(); // `${worker}:${step key}:${node id}` -> raw pads (4 bytes a frame) of a node's path from its step's root, or {asked: time}
    this.parentSlot = new Int32Array(CAP).fill(-1);
    this.keyAt = new Float64Array(CAP).fill(-1);
    this.index = new Map(); // (worker, step, id) -> slot
    this.mirrors = []; // per worker: the sparse mirror of its current step's search tree (lib/webminer/treeMirror.js)
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
    const was = this.enabled;
    this.enabled = !!on;
    if (!on) { this.clear(); this.workers = []; this.blocks = []; }
    if (on && !was && this.roster) this.build(this.roster);
    this.notify();
    if (!on) { this.version++; this.subs.forEach((f) => f(this.version)); }
  }

  /** A round starts: a new template, `count` workers each on `starts[i]`. */
  newRound({ template, starts, parentRaw }) {
    this.roster = { template, parentRaw: parentRaw || null, t0: Date.now() / 1000, workers: starts.map((start) => liteWorker(start)) };
    if (!this.enabled) return;
    this.build(this.roster);
    this.notify();
  }

  /** The views' state for the round in `r` (this.roster): what newRound sets up, plus where each worker's campaign is. */
  build(r) {
    const keep = { blocks: this.blocks, toasts: this.toasts };
    this.clear();
    this.blocks = keep.blocks;
    this.toasts = keep.toasts;
    this.round++;
    this.template = r.template;
    this.startKind = r.workers[0]?.start || 'parent';
    this.parentRaw = r.parentRaw;
    this.portable = new Map(); // converted start states for the game view (startState.js portableFor)
    this.t0 = r.t0;
    this.workers = r.workers.map((l, i) => ({
      i, cur: null, fps: 0, lastSim: null, nodesTotal: 0, best: null, bestPads: null, bestStep: null, prefix: null, root: null, keys: {}, found: false, runs: 0,
      ...l, steps: l.steps.map((x) => ({ ...x })),
    }));
  }

  /** Worker `i` begins a new search (a fresh campaign) from `start`. */
  restartWorker(i, start) {
    if (this.roster && this.roster.workers[i]) this.roster.workers[i] = liteWorker(start);
    const w = this.workers[i];
    if (!w) return;
    this.mirrors[i] = null;
    this.dropPaths(i);
    Object.assign(w, { start, line: 'loading the game…', steps: [], cur: null, stepNo: 0, played: 0, keysBase: 0, starsBase: 0, fps: 0, lastSim: null, nodesTotal: 0, best: null, bestPads: null, bestStep: null, prefix: null, root: null, keys: {} });
    this.notify();
  }

  /** A campaign log line of worker `i`. */
  log(i, line) {
    const ev = parseLog(line);
    const l = this.roster && this.roster.workers[i];
    if (l) logInto(l, line, ev); // kept while the visualisations are off too (a few lines a step)
    if (!this.enabled) return;
    const w = this.workers[i];
    if (!w) return;
    logInto(w, line, ev);
    if (ev.kind === 'step') w.cur = null;
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
      w.cur = { key: m.key, n: w.curN, step: m.step, t: 0, nodes: 0, sim: 0, best: null };
      w.keys[w.curN & 4095] = m.key;
      this.dropPaths(i);
      w.lastSim = null;
      w.best = null;
      w.bestPads = null;
      w.prefix = m.prefix || null;
      // The step's root is the cloud's first point.
      this.add(i, w.cur.n, 0, -1, w.played, m.step.pos, w.starsBase, m.step.level, m.step.area, 0);
    }
    if (m.prefix) w.prefix = m.prefix;
    const c = w.cur;
    c.t = m.t; c.nodes = m.nodesTotal; c.sim = m.simFrames;
    w.nodesTotal = m.nodesTotal;
    if (w.lastSim && m.simFrames >= w.lastSim.sim && now > w.lastSim.at) w.fps = (m.simFrames - w.lastSim.sim) / ((now - w.lastSim.at) / 1000);
    w.lastSim = { sim: m.simFrames, at: now };
    for (const n of m.nodes) {
      this.add(i, c.n, n[0], n[1], w.played + n[2], [n[3], n[4], n[5]], n[6], n[7], n[8], n[9]);
    }
    if (m.best) {
      const prev = w.best;
      w.best = { ...m.best, frameAbs: w.played + m.best.frame };
      c.best = w.best;
      if (m.pads) { w.bestPads = m.pads; w.bestStep = { played: w.played, key: m.key, prefix: w.prefix }; }
      const p = m.best.stars + m.best.keys;
      if (m.pads) {
        if (!prev || p > prev.stars + prev.keys) this.toast(`worker ${i + 1}: ${m.best.stars} ★${m.best.keys ? ` ${m.best.keys} 🔑` : ''} reached at frame ${w.played + m.best.frame}`, 'best');
      }
    }
    if (m.cov) {
      let lead = null;
      for (const x of this.workers) if (x.cur && (!lead || x.stepNo > lead.stepNo)) lead = x;
      this.cover.add(i, m.key, m.cov, lead ? lead.cur.key : m.key, this.workers.length, lead ? lead.stepNo : w.stepNo);
    }
    if (m.clip) this.addClip(i, m);
    this.notify();
  }

  add(w, step, id, parent, frame, pos, stars, level, area, flags) {
    const key = (w * 4096 + (step & 4095)) * 1e8 + id;
    // A node can come twice (a manual node the page added itself, then the tree's own sample of it): the same slot.
    const had = this.index.get(key);
    const slot = had ?? this.n % CAP;
    if (had === undefined) {
      const old = this.keyAt[slot];
      if (old >= 0) this.index.delete(old);
      this.keyAt[slot] = key;
      this.index.set(key, slot);
      this.n++;
    }
    this.pos[slot * 3] = pos[0]; this.pos[slot * 3 + 1] = pos[1]; this.pos[slot * 3 + 2] = pos[2];
    this.frame[slot] = frame;
    this.stars[slot] = stars;
    this.worker[slot] = w;
    this.level[slot] = level;
    this.area[slot] = area;
    this.flags[slot] = had === undefined ? flags : flags | (this.flags[slot] & 2);
    this.nodeId[slot] = id;
    this.stepOf[slot] = step & 4095;
    this.parentSlot[slot] = parent >= 0 ? this.index.get((w * 4096 + (step & 4095)) * 1e8 + parent) ?? -1 : -1;
    this.cloudChanged = true;
    return slot;
  }

  /** Worker `i`'s search-tree edges (binary, lib/webminer/treeMirror.js): the page's sparse mirror of the tree. */
  addEdges(i, key, buf) {
    if (!this.enabled) return;
    const m = (this.mirrors[i] ??= new TreeMirror());
    m.add(key, buf);
    this.edgeBytes = (this.edgeBytes || 0) + buf.byteLength;
  }

  /** The raw pads (4 bytes a frame) of node `id`'s path from the step `key`'s root of worker `w`, from the mirror; null when it isn't there. */
  nodePath(w, key, id) {
    return this.mirrors[w]?.path(key, id) || null;
  }

  /** The mirror's size now: {edges, bytes} over the workers. */
  mirrorStats() {
    let edges = 0, bytes = 0;
    for (const m of this.mirrors) if (m) { edges += m.size; bytes += m.bytes + m.size * 96; }
    return { edges, bytes, fed: this.edgeBytes || 0 };
  }

  // ----- the highlight reel -----
  /** A worker reported a highlight (m.clip: a new place or a record) of its current step: queue it with its path. */
  addClip(i, m) {
    const c = m.clip;
    const w = this.workers[i];
    const raw = this.nodePath(i, m.key, c.id);
    if (!raw || !raw.length) return;
    const where = levelName(c.level);
    const fr = w.played + c.frame;
    const label = c.kind === 'place'
      ? `worker ${i + 1} reached a new place · frame ${fr.toLocaleString()} · ${where}`
      : `worker ${i + 1}: faster route, −${c.gain} frames · frame ${fr.toLocaleString()} · ${where}`;
    const clip = { w: i, step: w.cur.n, key: m.key, id: c.id, kind: c.kind, gain: c.gain, frame: fr, played: w.played, raw, rootRaw: w.root && w.root.key === m.key ? w.root.raw : null, label, at: Date.now() };
    if (!this.wanted(c.kind, c.gain)) return;
    this.reel.clips++;
    if (this.reel.follow != null && this.reel.follow !== i) return;
    const q = this.reel.queue;
    q.push(clip);
    while (q.length > REEL_QUEUE) { const k = q.findIndex((x) => x.kind !== 'place'); q.splice(k < 0 ? 0 : k, 1); }
    this.reelKick();
  }

  /** Starts the next clip when nothing is playing (or only a best-path replay, which the reel replaces). */
  reelKick() {
    const g = this.game;
    if (this.reel.on && !this.found && !g.kind) this.reelAdvance();
  }

  /** The next queued clip becomes the game; false when the reel is off or empty. Called when a clip ends (never mid-play). */
  reelAdvance() {
    const r = this.reel;
    if (!r.on || this.found) return false;
    while (r.queue.length && Date.now() - r.queue[0].at > REEL_STALE_MS) r.queue.shift();
    const c = r.queue.shift();
    if (!c) return false;
    this.game = { id: this.game.id + 1, kind: 'clip', w: c.w, clip: c };
    this.hl = { w: c.w, step: c.step, id: c.id };
    this.notify();
    return true;
  }

  setSettings(patch) {
    Object.assign(this.settings, patch);
    try { localStorage.setItem(SETTINGS_KEY, JSON.stringify(this.settings)); } catch { /* private window */ }
    const s = this.settings;
    this.reel.queue = this.reel.queue.filter((c) => this.wanted(c.kind, c.gain, s));
    this.notify();
  }

  /** Whether the settings let a highlight of `kind` (with record gain `gain`) into the reel. */
  wanted(kind, gain, s = this.settings) {
    return kind === 'place' ? s.places : s.records && gain >= s.minGain;
  }

  setReel(patch) {
    Object.assign(this.reel, patch);
    if (patch.follow !== undefined) this.reel.queue = this.reel.queue.filter((c) => patch.follow == null || c.w === patch.follow);
    if (this.reel.on) this.reelKick();
    this.notify();
  }

  /** What the game view needs to play clip `c` (same shape as nodePlay's answer). */
  clipPlay(c) {
    return { key: c.key, played: c.played, best: null, rootRaw: c.rootRaw, raw: c.raw };
  }

  /** The cloud slot of a node (or -1). */
  slotOf(w, step, id) {
    return this.index.get((w * 4096 + (step & 4095)) * 1e8 + id) ?? -1;
  }

  /** Whether the slot's node belongs to its worker's current search tree (the one that can answer for it). */
  isCurrent(slot) {
    const w = this.workers[this.worker[slot]];
    return !!w?.cur && (w.cur.n & 4095) === this.stepOf[slot];
  }

  /**
   * A human-played segment (hybrid mining) that now is a node of the tree: `info` = {w, step, nid, parentSlot, pos, level, area, stars,
   * frame, trail: [x, y, z, ...]}. The node is a point of the cloud (flag bit 1 = manual) and the cloud highlights it.
   */
  playedSegment(info) {
    if (!this.enabled) return;
    const slot = info.nid != null ? this.add(info.w, info.step, info.nid, -1, info.frame, info.pos, info.stars, info.level, info.area, 2) : -1;
    if (slot >= 0 && info.parentSlot != null && info.parentSlot >= 0) this.parentSlot[slot] = info.parentSlot;
    this.manual = { ...info, slot, at: Date.now() };
    this.cloudChanged = true;
    this.notify();
  }

  setGoal(goal) { this.goal = goal; this.cloudChanged = true; this.notify(); }

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

  /** Forgets worker `i`'s node inputs (asked or received): its step changed, so they can't be played any more. */
  dropPaths(i) {
    const pre = `${i}:`;
    for (const k of this.paths.keys()) if (k.startsWith(pre)) this.paths.delete(k);
  }

  /** The step's root perturbation and sync tag (hybrid mining): {tag, pert (JSON text)} as sent with the step's start state. */
  setPert(i, key, json) {
    const w = this.workers[i];
    if (!w || !this.enabled) return;
    try { w.rootPert = { key, ...JSON.parse(json) }; } catch { /* unreadable */ }
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
    const mir = this.nodePath(w, key, nid);
    if (mir) return { ...base, raw: mir };
    const pk = `${w}:${key}:${nid}`;
    const got = this.paths.get(pk);
    if (got instanceof Uint8Array) return { ...base, raw: got };
    // Asked and not answered within a few seconds (the worker answers at its next sample, about once a second): ask again.
    if (!got || performance.now() - got.asked > ASK_AGAIN_MS) {
      if (!this.requester || !this.requester(w, key, nid)) return { error: 'only a search\'s best node has its inputs here; switch on worker sync (the button above Start) to play any node' };
      this.paths.set(pk, { asked: performance.now() });
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
