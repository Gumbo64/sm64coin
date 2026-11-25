// The Auto-mine controller: one per page load (a module singleton, `miner`), so mining goes on while the
// visitor browses the site's other pages. It owns the workers (one RRT campaign each, lib/webminer/worker.js),
// the round / submit logic (a new tip restarts the workers on the new template), the reset policy, the
// settings (address, workers, reset mode, visualisations: remembered in localStorage), the stats and log, and
// the visualisations' data hub. Views (pages/AutoMine.jsx, the header's status light) subscribe through
// lib/webminer/useMiner.js. Mining starts and stops only through start() / stop() (the page's buttons),
// and stops when the tab closes.
import { daemon } from '../rpc.js';
import { getCycle } from '../replay/client.js';
import { hexToBytes, seal, withTimestamp, templateTimestamp } from '../proof.js';
import { frames as fmtFrames } from '../format.js';
import { boardSize, setVizFlag, postMail } from './board.js';
import { RESET_MODES, initialStart, afterFailure, restartAt } from '../resetPolicy.js';
import { announceMined } from '../payout.js';
import { VizHub } from '../../pages/automine/hub.js';

export const isAddress = (a) => a.startsWith('SM6') && a.length > 90;
const now = () => Date.now() / 1000;
// Workers can sync their searches (SharedArrayBuffer) only in a cross-origin-isolated page: the
// service worker coi-sw.js sets the headers GitHub Pages can't (one reload after enabling it).
export const isolated = typeof SharedArrayBuffer !== 'undefined' && globalThis.crossOriginIsolated === true;
export async function enableSync() {
  await navigator.serviceWorker.register(`${import.meta.env.BASE_URL}coi-sw.js`);
  await navigator.serviceWorker.ready;
  window.location.reload();
}
const SEND_AHEAD = 100; // the nodes' future-timestamp window (120 s), with a margin
const ls = (k, d) => { try { return localStorage.getItem(k) ?? d; } catch { return d; } };
const lsSet = (k, v) => { try { localStorage.setItem(k, v); } catch { /* private window */ } };

// Seconds of the frames/s moving average. Workers report once per search round (a round can take
// tens of seconds on a loaded machine), so the window is long and the readout only drops to 0 when
// no worker has reported for RATE_STALE seconds.
export const RATE_WINDOW = 60;
export const RATE_STALE = 120;
/** "1.37 M", "412 k", "950": a frame rate (or count) with k / M units. */
export const fmtRate = (x) => (x >= 1e6 ? `${(x / 1e6).toFixed(x >= 1e7 ? 1 : 2)} M` : x >= 1e3 ? `${(x / 1e3).toFixed(x >= 1e5 ? 0 : 1)} k` : `${Math.round(x)}`);

/** Frames/s from the workers' cumulative per-step frame counts (a count that drops is a new step). */
class FrameRate {
  constructor() { this.last = []; this.cum = 0; this.samples = []; }
  reset(i) { this.last[i] = 0; }
  add(i, sim) {
    const prev = this.last[i] || 0;
    this.cum += sim >= prev ? sim - prev : sim;
    this.last[i] = sim;
    const t = performance.now();
    this.samples.push([t, this.cum]);
    const cut = t - RATE_WINDOW * 1000;
    while (this.samples.length > 2 && this.samples[1][0] < cut) this.samples.shift();
  }
  clear() { this.last = []; this.samples = []; }
  fps() {
    const n = this.samples.length;
    if (n < 2) return 0;
    const [t0, c0] = this.samples[0], [t1, c1] = this.samples[n - 1];
    // No report for a while (stopped, or between steps): the average decays instead of freezing.
    const end = Math.max(t1, performance.now());
    return end - t0 > 500 && performance.now() - t1 < RATE_STALE * 1000 ? (c1 - c0) / ((end - t0) / 1000) : 0;
  }
}

class MinerController {
  constructor() {
    this.hub = new VizHub();
    this.rate = new FrameRate();
    this.subs = new Set();
    const q = new URLSearchParams(window.location.search).get('address');
    this.address = q && isAddress(q.trim()) ? q.trim() : ls('sm64coin.address', '');
    this.count = Math.max(1, Math.min(8, (navigator.hardwareConcurrency || 2) - 1));
    const m = ls('sm64coin.resetMode', 'when-stuck');
    this.mode = RESET_MODES.includes(m) ? m : 'when-stuck';
    this.vizOn = ls('sm64coin.viz', '1') !== '0';
    this.running = false;
    this.log = [];
    this.workers = [];
    this.stats = { rounds: 0, found: 0, accepted: 0 };
    this.phase = 'idle'; // idle | loading | mining | found | held | submitting
    this.flash = null; // {kind: 'accepted' | 'rejected', until}
    this.error = null; // {text, until}
    this.c = null;
    this.timer = null;
    this.snap = this.makeSnap();
    this.hub.setEnabled(this.vizOn);
    this.hub.requester = (w, key, nid) => this.askNode(w, key, nid);
    window.addEventListener('beforeunload', () => this.stop());
  }

  // ----- subscription -----
  subscribe = (fn) => { this.subs.add(fn); return () => this.subs.delete(fn); };
  getSnapshot = () => this.snap;
  makeSnap() {
    return { address: this.address, count: this.count, mode: this.mode, vizOn: this.vizOn, running: this.running, log: this.log, workers: this.workers, stats: this.stats, fps: this.fps(), status: this.status() };
  }
  /** Wakes the views: at most 4 times a second, at once when `now`. */
  emit(now = false) {
    const fire = () => { this.timer = null; this.snap = this.makeSnap(); this.subs.forEach((f) => f()); };
    if (now) { clearTimeout(this.timer); fire(); } else if (!this.timer) this.timer = setTimeout(fire, 250);
  }

  /** The header light's state: {kind, label, title}. kinds: idle loading mining found accepted error reset. */
  status() {
    const st = this.statusKind();
    return { ...st, on: this.running };
  }

  /** Smoothed total simulated frames per second (moving average over RATE_WINDOW s); mock workers fall back to the hub's. */
  fps() {
    return this.rate.fps() || this.hub.workers.reduce((s, w) => s + (w.fps || 0), 0);
  }

  statusKind() {
    const t = Date.now();
    const n = this.count;
    const fps = this.fps();
    const rate = fps > 0 ? `, ${fmtRate(fps)} fr/s` : '';
    if (this.flash && this.flash.until > t) return this.flash.kind === 'accepted' ? { kind: 'accepted', label: '★ block!', title: 'Block accepted' } : { kind: 'error', label: 'rejected', title: 'The block was rejected' };
    if (!this.running) return { kind: 'idle', label: 'off', title: 'Auto-miner off (click to open the Auto-mine page)' };
    if (this.error && this.error.until > t) return { kind: 'error', label: 'error', title: `Auto-miner error: ${this.error.text}` };
    if (this.phase === 'found') return { kind: 'found', label: 'run found', title: 'A run was found: sealing it' };
    if (this.phase === 'held') return { kind: 'found', label: 'waiting', title: 'Run found: waiting until the time limit has grown to it' };
    if (this.phase === 'submitting') return { kind: 'found', label: 'submitting', title: 'Submitting the block' };
    if (this.phase === 'loading') return { kind: 'loading', label: 'loading', title: 'Auto-miner preparing (game, state, template)' };
    const reset = this.c && this.c.status && this.c.status.some((s) => s.start === 'power-on');
    if (reset) return { kind: 'reset', label: 'reset cycle', title: `Auto-miner: ${n} worker${n === 1 ? '' : 's'}, a reset-cycle (power-on) attempt${rate}` };
    return { kind: 'mining', label: fps > 0 ? `${fmtRate(fps)} fr/s` : `mining ${n}`, title: `Auto-miner mining: ${n} worker${n === 1 ? '' : 's'}${rate}` };
  }

  setPhase(p) { this.phase = p; this.emit(true); }
  say(m) { this.log = [`${new Date().toLocaleTimeString()}  ${m}`, ...this.log].slice(0, 200); this.emit(); }
  fail(text) { this.error = { text, until: Date.now() + 20000 }; this.emit(true); setTimeout(() => this.emit(true), 20500); }

  // ----- settings -----
  setAddress(a) { this.address = a; if (isAddress(a)) lsSet('sm64coin.address', a); this.emit(true); }
  setCount(n) { this.count = Math.max(1, Math.min(32, Number(n) || 1)); this.emit(true); }
  setMode(m) { this.mode = m; lsSet('sm64coin.resetMode', m); this.emit(true); }
  setVizOn(on) {
    this.vizOn = on;
    lsSet('sm64coin.viz', on ? '1' : '0');
    this.hub.setEnabled(on);
    const c = this.c;
    if (c) {
      if (c.board) setVizFlag(c.board, c.count, on); // live, through the shared board
      for (const w of c.pool) if (w) w.postMessage({ type: 'viz', on });
    }
    this.emit(true);
  }
  /** Asks worker `w`'s search for the inputs of node `nid` of its step `key` (through the board: isolated pages only); false without one. */
  askNode(w, key, nid) {
    const c = this.c;
    if (!c || !c.board) return false;
    c.reqs = [...(c.reqs || []).slice(-40), { id: (c.reqSeq = (c.reqSeq || 0) + 1), slot: w, key, nid }];
    return postMail(c.board, c.count, { reqs: c.reqs });
  }
  /** The Auto-mine page is on screen (the views redraw); off screen the data still collects. */
  setViewing(on) { this.hub.live = on; if (on) this.hub.notify(); }

  // ----- mining -----
  /** Development: fake workers (pages/automine/mock.js) on the chain's real tip; nothing is mined or submitted. */
  async startMock({ count = 3, runAfter = 0 } = {}) {
    this.stop();
    this.noSubmit = true;
    const c = { stopped: false, pool: [], attempt: 0, template: null, status: Array.from({ length: count }, () => ({ start: 'parent' })), count };
    this.c = c;
    this.count = count;
    this.running = true;
    this.phase = 'loading';
    this.emit(true);
    const m = await import('../../pages/automine/mock.js');
    c.template = await m.mockRound(this.hub, count);
    if (c.stopped) return;
    this.workers = c.status.map((s, i) => ({ salt: i, line: 'mock', runs: 0, fails: 0, start: s.start }));
    this.stats = { ...this.stats, rounds: this.stats.rounds + 1 };
    c.tick = setInterval(() => this.emit(), 2000);
    c.mockStop = m.startMock(this.hub, count, { runAfter });
    this.setPhase('mining');
  }

  stop() {
    const c = this.c;
    if (!c) return;
    c.stopped = true;
    if (c.mockStop) c.mockStop();
    for (const w of c.pool) if (w) w.terminate();
    c.pool = [];
    clearInterval(c.watch);
    clearInterval(c.tick);
    this.c = null;
    this.running = false;
    this.workers = [];
    this.rate.clear();
    this.hub.roster = null; // nothing to show when the visualisations are switched on after this
    this.phase = 'idle';
    this.error = null;
    this.emit(true);
  }

  start() {
    if (!isAddress(this.address)) { this.say('enter your SM64… address first'); return; }
    this.stop();
    this.noSubmit = false; // a mock session (?vizmock=1) earlier in this page must not keep real runs from being submitted
    const c ={ stopped: false, pool: [], attempt: 0, template: null, status: [], count: this.count };
    this.c = c;
    this.running = true;
    this.phase = 'loading';
    this.error = null;
    // A new tip: the template is stale, start over on the new one.
    c.watch = setInterval(async () => {
      try {
        // A parent-only template turns "either" at sm64_power_on_at: refresh it to learn that.
        if (c.template && c.restartAt && now() >= c.restartAt && c.pool.some(Boolean)) {
          c.restartAt = 0;
          this.say('the stall time passed: refreshing the template (power-on start now allowed)');
          this.round(c);
          return;
        }
        const i = await daemon('get_info');
        if (c.template && i.top_block_hash !== c.template.prev_hash && c.pool.some(Boolean)) {
          this.say(`new block ${i.height - 1} on the chain: restarting on the new template`);
          this.round(c);
        }
      } catch { /* node hiccup */ }
    }, 5000);
    c.tick = setInterval(() => this.emit(), 1000); // the light's frames/s
    this.emit(true);
    this.round(c);
  }

  async round(c) {
    const { hub } = this;
    const address = this.address;
    const count = c.count;
    const say = (m) => this.say(m);
    c.pool.forEach((w) => w && w.terminate());
    c.pool = [];
    if (c.stopped) return;
    this.setPhase('loading');
    let t;
    try {
      t = await daemon('get_block_template', { wallet_address: address, reserve_size: 0 });
    } catch (e) {
      say(`no template: ${e}; retrying in 10 s`);
      this.fail(`no template: ${e}`);
      setTimeout(() => this.round(c), 10000);
      return;
    }
    if (c.stopped) return;
    c.template = t;
    c.attempt += 1;
    this.stats = { ...this.stats, rounds: this.stats.rounds + 1 };
    const first = initialStart(this.mode, t.sm64_start, t.sm64_parent_stars);
    say(`block ${t.height}: reset policy ${this.mode}, chain start ${t.sm64_start}: starting from ${first.start} (${first.why})`);
    const start = first.start;
    c.restartAt = restartAt(this.mode, t.sm64_start, t.sm64_power_on_at || 0);
    let parentPads = '';
    let parentState = null; // the parent's end state from the node (it has it cached)
    // Parent start state (also fetched for "either": a worker may fall back to power-on later,
    // and the parent search needs it first).
    if (start === 'parent') {
      try {
        const s = await daemon('get_sm64_state', { hash: t.prev_hash });
        const gz = new Blob([hexToBytes(s.state_gz)]).stream().pipeThrough(new DecompressionStream('gzip'));
        parentState = await new Response(gz).arrayBuffer();
        say(`block ${t.height}: the parent's end state from the node (${t.sm64_parent_stars} stars, ${t.sm64_parent_keys} keys)`);
      } catch (e) {
        // An older node, or the state isn't cached: rebuild it from the cycle.
        say(`block ${t.height}: rebuilding the parent's end state (${t.sm64_parent_stars} stars, ${t.sm64_parent_keys} keys)`);
        try {
          parentPads = (await getCycle(t.prev_hash)).played_pads;
        } catch (e2) {
          say(`no cycle: ${e2}; retrying in 10 s`);
          this.fail(`no cycle: ${e2}`);
          setTimeout(() => this.round(c), 10000);
          return;
        }
      }
    }
    if (c.stopped || c.template !== t) return;
    this.error = null;
    say(`block ${t.height}: ${count} worker${count === 1 ? '' : 's'} searching (start ${start}, limit ${fmtFrames(t.sm64_max_frames)} now)`);
    const status = Array.from({ length: count }, (_, i) => ({ salt: c.attempt * 1000 + i, line: 'loading the game…', runs: 0, fails: 0, start }));
    c.status = status;
    const show = () => { this.workers = status.map((s) => ({ ...s })); this.emit(); };
    show();
    hub.newRound({ template: t, starts: status.map((s) => s.start), parentRaw: parentState });
    // The block's first castle step is the same for every worker: the first to finish it shares
    // the outcome (and the files it left), the others drop their own castle search and go on from
    // it with their own salts. Per start (a power-on worker's castle differs from a parent one's).
    const shared = {};
    // Checkpoint sync between the workers (their searches' best checkpoints, see rrt::mine):
    // needs a SharedArrayBuffer, i.e. a cross-origin-isolated page.
    const board = isolated ? new SharedArrayBuffer(boardSize(count)) : null;
    if (board) { c.board = board; c.count = count; setVizFlag(board, count, this.vizOn); } else c.board = null;
    const share = (i, m) => {
      const st = status[i].start;
      if (shared[st]) return;
      shared[st] = { step: m.step, files: m.files };
      say(`worker ${i + 1} finished the castle step: shared with the other workers`);
      for (let j = 0; j < count; j++) {
        if (j !== i && status[j].start === st && !status[j].gotShared && c.pool[j]) {
          c.pool[j].terminate();
          status[j].gotShared = true;
          launch(j);
        }
      }
    };
    // Each worker searches on its own: one that ends without a run (or fails) starts again with
    // a new salt, after a pause that grows if it keeps failing; the first run found wins.
    // One worker per slot: a launch replaces whatever the slot had (a pending relaunch, or a worker that share()
    // restarted while this slot's relaunch timer was still pending), and messages from a replaced worker are ignored.
    const launch = (i) => {
      clearTimeout(status[i].relaunch);
      status[i].relaunch = null;
      if (c.stopped || c.template !== t || c.submitting === t) return;
      if (c.pool[i]) c.pool[i].terminate();
      const w = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
      c.pool[i] = w;
      const sh = shared[status[i].start] || null;
      hub.restartWorker(i, status[i].start);
      this.rate.reset(i);
      if (sh) status[i].gotShared = true;
      w.onmessage = (e) => {
        const m = e.data;
        if (c.stopped || c.template !== t || c.pool[i] !== w) return;
        if (this.phase === 'loading') this.setPhase('mining');
        if (m.type === 'log') {
          status[i].line = m.line;
          hub.log(i, m.line);
          show();
          return;
        }
        if (m.type === 'progress') { this.rate.add(i, m.sim); return; }
        if (m.type === 'vizstate') { hub.setRoot(i, m.key, m.state); return; }
        if (m.type === 'vizedges') { hub.addEdges(i, m.key, m.buf); return; }
        if (m.type === 'vizpath') { hub.setPath(i, m.key, m.id, m.flat); return; }
        if (m.type === 'viz') {
          const f = /"sim_frames":(\d+)/.exec(m.line);
          if (f) this.rate.add(i, +f[1]);
          hub.ingest(i, m.line);
          return;
        }
        if (m.type === 'share') {
          share(i, m);
          return;
        }
        w.terminate();
        const r = m.result;
        if (m.type === 'result' && r.status === 'found') {
          this.stats = { ...this.stats, found: this.stats.found + 1 };
          hub.foundRun(i, { ...r, start: status[i].start }, t);
          say(`worker ${i + 1}: run found, ${r.frames} frames (${fmtFrames(r.frames)}), ${(r.sim_frames / 1e6).toFixed(0)} M frames searched`);
          c.pool.forEach((x) => x && x.terminate());
          c.pool = [];
          // No relaunch while this run is submitted: the other slots' pending relaunch timers would start workers
          // on this template again (and a run they found would be submitted a second time). submit() ends with round().
          c.submitting = t;
          status.forEach((s) => { clearTimeout(s.relaunch); s.relaunch = null; });
          this.setPhase('found');
          this.submit(c, t, { ...r, start: status[i].start });
          return;
        }
        const failed = m.type === 'error';
        status[i].fails = failed ? status[i].fails + 1 : 0;
        status[i].runs += 1;
        if (hub.workers[i]) hub.workers[i].runs = status[i].runs;
        status[i].line = failed ? `failed: ${m.message}` : `no run (${r.why}); starting again`;
        if (failed) { say(`worker ${i + 1} failed: ${m.message.slice(0, 200)}`); this.fail(`worker ${i + 1}: ${m.message.slice(0, 120)}`); }
        status[i].salt += count;
        // A parent-start search that ends without a run on an "either" template: reset cycle.
        if (!failed && status[i].start === 'parent' && afterFailure(this.mode, t.sm64_start)) {
          status[i].start = 'power-on';
          status[i].gotShared = false;
          say(`worker ${i + 1}: no run from the parent (${r.why}); reset policy ${this.mode}: next attempt from power-on (reset cycle)`);
        }
        show();
        status[i].relaunch = setTimeout(() => launch(i), Math.min(60, 2 ** status[i].fails) * 1000);
      };
      // A worker that never ran (its module failed to load) is a failed search too, not a dead slot.
      w.onerror = (ev) => { ev.preventDefault(); w.onmessage({ data: { type: 'error', message: ev.message || 'the worker failed to start' } }); };
      w.postMessage({
        type: 'mine',
        request: { start: status[i].start, parent_pads: parentPads, seed_hash: t.sm64_seed_hash, retarget_difficulty: t.sm64_retarget_difficulty, salt: status[i].salt, threads: 1 },
        parentState: status[i].start === 'parent' && parentState ? parentState.slice(0) : null,
        viz: true, // the sampler is always on: with the feed off it only reports a frame count
        vizOn: this.vizOn,
        shared: sh,
        board: board && { sab: board, slot: i, slots: count },
      });
    };
    for (let i = 0; i < count; i++) launch(i);
  }

  async submit(c, t, r) {
    const say = (m) => this.say(m);
    const { hub } = this;
    try {
      let validAt = 0;
      try {
        validAt = (await daemon('get_block_template', { wallet_address: this.address, reserve_size: 0, sm64_frames: r.frames })).sm64_valid_at || 0;
      } catch { /* the node decides */ }
      for (let held = false; validAt - now() > SEND_AHEAD; held = true) {
        if (c.stopped || c.template !== t) return;
        this.setPhase('held');
        if (!held) say(`the run is over the time limit: valid at ${new Date(validAt * 1000).toLocaleTimeString()}, held until then (a new block first makes it worthless)`);
        await new Promise((res) => setTimeout(res, Math.min(30, validAt - now() - SEND_AHEAD) * 1000));
        // The watch only restarts rounds that have workers (none while holding): look for a new tip here, or the
        // miner would sit idle on a worthless run until its valid time.
        let tip = null;
        try { tip = (await daemon('get_info')).top_block_hash; } catch { /* node hiccup: look again next time */ }
        if (c.stopped || c.template !== t) return;
        if (tip && tip !== t.prev_hash) {
          say('a new block landed while the run was held; the run was for the old tip');
          hub.submission('stale', { height: t.height, frames: r.frames });
          return;
        }
      }
      this.setPhase('submitting');
      const info = await daemon('get_info');
      if (info.top_block_hash !== t.prev_hash) {
        say('a new block landed first; the run was for the old tip');
        hub.submission('stale', { height: t.height, frames: r.frames });
        return;
      }
      let ts = Math.max(Math.floor(now()), validAt, templateTimestamp(t.blocktemplate_blob));
      // A reset cycle (power-on on an "either" template) is judged against an earlier limit and is valid from sm64_power_on_at.
      if (r.start === 'power-on' && t.sm64_start === 'either' && t.sm64_power_on_at) ts = Math.max(ts, t.sm64_power_on_at);
      if (!this.noSubmit) await daemon('submit_block', [seal(withTimestamp(t.blocktemplate_blob, ts), hexToBytes(r.proof))]);
      this.stats = { ...this.stats, accepted: this.stats.accepted + 1 };
      hub.submission('accepted', { height: t.height, frames: r.frames });
      announceMined(t.height, t.expected_reward);
      this.flash = { kind: 'accepted', until: Date.now() + 8000 };
      setTimeout(() => this.emit(true), 8100);
      say(`★ BLOCK ${t.height} SUBMITTED: ${r.frames} frames, to ${this.address.slice(0, 12)}…`);
    } catch (e) {
      say(`block rejected: ${e}`);
      hub.submission('rejected', { height: t.height, frames: r.frames, note: String(e) });
      this.flash = { kind: 'rejected', until: Date.now() + 10000 };
      setTimeout(() => this.emit(true), 10100);
    } finally {
      if (!c.stopped) this.round(c);
    }
  }
}

export const miner = new MinerController();
window.__sm64miner = miner; // for the e2e scripts
if (import.meta.hot) import.meta.hot.dispose(() => miner.stop());
