// Plays a whole cycle (up to and including one block) in the visual engine, as
// a subclass of sm64-viewer's Player (shared through sm64-replay-ui): the
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
import { Player, Trail } from 'sm64-replay-ui';
import { Perturbation, perturbReady, readProgress } from './perturb.js';
import { captureState, restoreState } from './savestate.js';
import { muteDrawing } from './cycle.js';

const SNAP_EVERY = 240; // 8 s
const MAX_SNAPS = 200;

export class CyclePlayer extends Player {
  /** pads: the whole cycle's played pads; start: the frame this block starts at. */
  constructor(pads, onChange, { start, task, won }) {
    let self = null;
    super(pads, (v) => onChange({ ...v, verify: self.verify() }));
    self = this;
    this.S = start;
    this.won = won;
    this.task = task;
    this.pert = null; // made in attach(), once the WASM is loaded
    this.pertStart = null;
    this.trail = new Trail(SNAP_EVERY, MAX_SNAPS);
    this.okTo = start; // block frames [start, okTo) verified, contiguously
    this.bad = null; // the first frame that did not match the perturbation
    this.wonEnd = null;
    this.startProgress = null;
    this.unmute = null;
  }

  async attach(container) {
    await perturbReady();
    this.pert = new Perturbation(this.task);
    this.pertStart = this.pert.toBytes();
    await super.attach(container);
    const e = this.engine;
    e.canvas = this.canvas;
    this.trail.add(0, captureState(e)); // power-on
  }

  dispose() {
    super.dispose();
    this.pert?.free();
  }

  verify() {
    return { start: this.S, okTo: this.okTo, bad: this.bad, wonEnd: this.wonEnd, total: this.pads.length };
  }

  play() {
    if (this.ended) { this.seek(this.S); this.playing = true; this.dirty = true; return; }
    super.play();
  }

  // --- save states ---
  // The viewer's Player keeps a Trail of states by frame (lib/trail.js, shared
  // with sm64-viewer, which keeps portable states there) and seeks both ways
  // through it. Here a state is {mem (lib/savestate.js: the engine's memory,
  // unchanged pages shared between states), pert (the perturbation)}.
  // Swap captureState/restoreState for the binds engine's save_state /
  // load_state when memory sharing is no longer needed.
  saveState() {
    const f = this.frame;
    const prev = this.trail.nearest(f - 1)?.state ?? null;
    const state = captureState(this.engine, f > this.S ? this.pert : null, prev);
    if (f === this.S) state.pert = this.pertStart;
    return state;
  }

  loadState(state, frame) {
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
      this.startProgress = this.startProgress ?? readProgress(this.engine);
      if (!this.trail.has(f)) this.trail.add(f, this.saveState()); // where the block starts
    }
    super.checkpoint();
  }

  stepGame(pad) {
    const i = this.frame;
    // While fast-forwarding, skip the drawing; the last frame is drawn.
    const quiet = this.seeking && i < this.seekTarget - 1;
    if (quiet && !this.unmute) this.unmute = muteDrawing(this.engine);
    else if (!quiet && this.unmute) { this.unmute(); this.unmute = null; }
    if (i < this.S) { this.engine.step_game(pad); return; }
    const [played] = this.pert.step(this.engine, pad);
    if (!played.equals(pad)) {
      if (this.bad === null) this.bad = i;
      this.playing = false;
    } else if (i <= this.okTo) this.okTo = Math.max(this.okTo, i + 1);
    if (i + 1 === this.pads.length && this.bad === null) this.wonEnd = this.won(this.startProgress, readProgress(this.engine));
  }
}
