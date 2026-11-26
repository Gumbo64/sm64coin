// Plays a whole cycle (up to and including one block) in the visual engine, as
// a subclass of sm64-binds-frontend's replay Player (`sm64-binds-frontend/replay`): the
// viewer's real-time play, speeds, frame steps, resolution control, picture
// freeze, MP4 export all come from there. What this adds:
//
// - Frames before the block (`start`) play the earlier blocks' played pads
//   as-is (already accepted). The block's own frames go through the
//   perturbation like the verifier does, and every played pad is checked
//   against the recorded one (`verify` in the view).
// - Seeking backwards: save states (lib/savestate.js: memory + the
//   perturbation's state) are taken every SNAP_EVERY frames as frames are
//   played; seeking restores the nearest earlier one and the viewer's
//   fast-forward replays from there. A state inside the block restores the
//   perturbation too, so seeking into the block still verifies.
import { Player, Trail } from 'sm64-binds-frontend/replay';
import { Perturbation, perturbReady, readProgress } from './perturb.js';
import { captureState, restoreState } from './savestate.js';
import { getBlockState, putBlockState } from './blockstates.js';
import { parentState } from './webminer/parentState.js';
import { portableFromRaw } from '../pages/automine/startState.js';

// A parent this far in is fetched from the node (get_sm64_state, ~7 MB) rather than fast-forwarded,
// when no cached state exists: below it the headless catch-up is quicker than the download.
export const NODE_STATE_FROM_FRAME = 20000;

const SNAP_EVERY = 240; // 8 s: about 150 states per 20 minutes of play
const MAX_SNAPS = 260;

export class CyclePlayer extends Player {
  /** pads: the whole cycle's played pads; start: the frame this block starts at. */
  constructor(pads, onChange, { start, task, won, boundaries = [] }) {
    let self = null;
    super(pads, (v) => onChange({ ...v, verify: self.verify() }));
    self = this;
    this.S = start;
    this.boundaries = boundaries; // [{frame, hash}]: where each earlier block of the run ended (the last is `start`), oldest first
    this.warmInfo = null; // how the start state was reached: {source, ms}
    this.won = won;
    this.task = task;
    this.pert = null; // made in attach(), once the WASM is loaded
    this.pertStart = null;
    this.workerTrail = false; // the headless worker's portable states are not this player's states (memory + perturbation)
    this.trail = new Trail(Math.max(SNAP_EVERY, Math.ceil(pads.length / (MAX_SNAPS - 40))), MAX_SNAPS);
    this.trackEvents = false; // no events list here (the timeline's stars come from the blocks): quiet seeks skip reading the game state
    this.okTo = start; // block frames [start, okTo) verified, contiguously
    this.bad = null; // the first frame that did not match the perturbation
    this.wonEnd = null;
    this.startProgress = null;
    this.unchecked = false; // in the block without the perturbation's state (after a headless catch-up)
  }

  async attach(container) {
    await perturbReady();
    this.pert = new Perturbation(this.task);
    this.pertStart = this.pert.toBytes();
    await super.attach(container);
    const e = this.engine;
    e.canvas = this.canvas;
    this.trail.add(0, captureState(e)); // power-on
    this.powerOnState = this.trail.states.get(0);
    await this.warm().catch((err) => console.warn('replay: reusing saved states failed, fast-forwarding instead:', err));
  }

  // Brings the game to the block's start (frame S) without replaying the cycle when it can:
  // the parent's end state from the cache (memory, then IndexedDB), else from the node, else by
  // a headless fast-forward that keeps every block's end state it passes for next time. Progress
  // is shown only while something slow really happens (the node's download, the fast-forward).
  async warm() {
    const S = this.S;
    const last = this.boundaries[this.boundaries.length - 1];
    if (!(S > 0) || !last || last.frame !== S) { this.warmInfo = { source: 'power-on', ms: 0 }; return; }
    const t0 = performance.now();
    const done = (source, state) => {
      this.landAtStart(state);
      this.warmInfo = { source, ms: Math.round(performance.now() - t0) };
      console.info(`replay: start state from ${source} in ${this.warmInfo.ms} ms`);
    };
    const hit = await getBlockState(this.powerOnState, last.hash);
    if (hit) { done(hit.from, hit.state); return; }
    const e = this.engine;
    const load = (portable) => {
      if (this.pristine) e.load_state(this.pristine);
      else restoreState(e, null, this.powerOnState);
      e.load_portable_state(portable);
    };
    const capture = () => captureState(e, null, this.powerOnState);
    if (S >= NODE_STATE_FROM_FRAME && e.portable_supported) {
      try {
        this.busy(0);
        const raw = await parentState(last.hash);
        const portable = await portableFromRaw(raw);
        load(portable);
        const state = capture();
        putBlockState(this.powerOnState, last.hash, state);
        done('node', state);
        return;
      } catch (err) { console.info('replay: no state from the node:', String(err?.message ?? err)); } finally { this.busy(null); }
    }
    if (!e.portable_supported || this.headlessFailed || !(await this.ensureHeadless())) return;
    try {
      let state = null;
      for (const b of this.boundaries) {
        this.busy(b.frame);
        const res = await this.headless.run({ type: 'goto', target: b.frame, maxStart: Infinity, every: 0, land: true, events: false }, (m) => this.busy(m.frame));
        if (res.cancelled) return;
        load(res.state);
        state = capture();
        putBlockState(this.powerOnState, b.hash, state);
      }
      done('fast-forward', state);
    } finally { this.busy(null); }
  }

  // After a start state from the cache or the node the worker's game is still at power-on: a seek
  // inside the block is stepped from the state at S here unless it is long enough for the worker
  // (about 4x faster, but starting from frame 0) to win.
  planHeadless(target) {
    const plan = super.planHeadless(target);
    if (plan && this.warmInfo && this.warmInfo.source !== 'fast-forward' && this.warmInfo.source !== 'power-on' && target > this.S) {
      const near = this.trail.nearest(target - 1);
      if (near && near.frame >= this.S && target - 1 - near.frame < plan.wt / 4) return null;
    }
    return plan;
  }

  busy(frame) {
    this.headlessBusy = frame !== null;
    this.headlessProgress = frame;
    this.dirty = true;
    this.emit();
  }

  /** The engine is loaded with `state` (the game at frame S, before its pad): make that the player's position. */
  landAtStart(state) {
    restoreState(this.engine, null, state);
    this.frame = this.S;
    this.state = this.engine.get_game_state();
    this.events = [];
    this.evThrough = this.S;
    this.rendered = false;
    this.unchecked = false;
    this.checkpoint(); // the perturbation's start, the start progress and the trail's state at S
    this.dirty = true;
    this.emit();
  }

  dispose() {
    super.dispose();
    this.pert?.free();
  }

  // Catching up (the first seek, long seeks) is done by the headless worker of
  // sm64-binds-frontend >= 2.3.0, to any frame: the cycle's played pads already
  // carry the pokes, so they reach the same game state without the perturbation.
  // A catch-up into the block skips its per-frame check (the perturbation's
  // state there is unknown, `unchecked`): frames are then played as recorded,
  // and the check runs again when the block is played from its start (the node
  // checked every frame when it accepted the block).
  headlessLimit() {
    return this.pads.length;
  }

  landHeadless(res) {
    super.landHeadless(res);
    this.unchecked = this.frame > this.S;
  }

  verify() {
    return { start: this.S, okTo: this.okTo, bad: this.bad, wonEnd: this.wonEnd, total: this.pads.length, unchecked: this.unchecked };
  }

  play() {
    if (this.ended) { this.seek(this.S); this.playing = true; this.dirty = true; return; }
    super.play();
  }

  // --- save states ---
  // The viewer's Player keeps a Trail of states by frame (lib/trail.js, from sm64-binds-frontend/replay, which keeps portable states there) and seeks both ways
  // through it. Here a state is {mem (lib/savestate.js: the engine's memory,
  // unchanged pages shared between states), pert (the perturbation)}.
  // Swap captureState/restoreState for the binds engine's save_state /
  // load_state when memory sharing is no longer needed.
  saveState() {
    const f = this.frame;
    const prev = this.trail.nearest(f - 1)?.state ?? null;
    // Unchecked (after a headless jump into the block): no perturbation state to keep.
    const state = captureState(this.engine, f > this.S && !this.unchecked ? this.pert : null, prev);
    if (f === this.S) state.pert = this.pertStart;
    return state;
  }

  loadState(state, frame) {
    // Our own states carry the perturbation inside the block; the worker's portable ones don't.
    this.unchecked = frame > this.S && !state.pert;
    restoreState(this.engine, this.pert, state);
    this.frame = frame;
    this.state = this.engine.get_game_state();
    this.events = this.events.filter((e) => e.frame <= frame);
    this.rendered = false;
    this.dirty = true;
  }

  checkpoint() {
    const f = this.frame;
    if (f === this.S) {
      this.pert.loadBytes(this.pertStart);
      this.unchecked = false;
      this.startProgress = this.startProgress ?? readProgress(this.engine);
      if (!this.trail.has(f)) this.trail.add(f, this.saveState()); // where the block starts
    }
    super.checkpoint();
  }

  stepGame(pad) {
    const i = this.frame;
    if (i < this.S || this.unchecked) { this.engine.step_game(pad); return; }
    const [played] = this.pert.step(this.engine, pad);
    if (!played.equals(pad)) {
      if (this.bad === null) this.bad = i;
      this.playing = false;
    } else if (i <= this.okTo) this.okTo = Math.max(this.okTo, i + 1);
    if (i + 1 === this.pads.length && this.bad === null) this.wonEnd = this.won(this.startProgress, readProgress(this.engine));
  }
}
