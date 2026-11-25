import { GamePad, GamepadButtons } from 'sm64-binds-frontend';
import { Perturbation, readProgress } from './perturb.js';
import { captureState, restoreState } from './savestate.js';
import { MAX_RUN_FRAMES } from './rules.js';
import { fastForward } from './cycle.js';
import { applyTouch } from './touchInput.js';

// Frames between periodic save states (5 s).
const SNAP_EVERY = 150;
// The most periodic save states kept (about 20 minutes of play).
const MAX_SNAPS = 240;
// Frames replayed per animation frame while catching up after a rewind.
const CATCH_UP_PER_TICK = 300;
// The last this many frames of a catch-up play at normal speed, so you see
// where you'll take over.
const CATCH_UP_TAIL = 90;

/**
 * A mining attempt in the browser, with the old miner's controls.
 *
 * The attempt starts from `start` (lib/cycle.js: power-on, or the parent
 * block's end state, as a save state). Every frame the player's pad goes
 * through the perturbation (lib/perturb.js, outside the game) and the pad
 * actually played is recorded. Every 5 s of play a save state is taken (the
 * engine's memory plus the perturbation: lib/savestate.js), so going back
 * restores the nearest earlier one and replays the recorded pads from there
 * (fast, then the last 3 s at normal speed), after which you play on. During a
 * catch-up, "take over" (or Start) cuts the run there.
 *
 * There is no hard time limit: the limit grows with time, so a run may go past
 * it (`limit`, see setLimit) and simply waits for it to become valid. Only
 * MAX_RUN_FRAMES (the longest limit any difficulty has) ends an attempt.
 *
 * `run(kill)` resolves with {pads, poked} when `won(startProgress, progress)`
 * first holds (that frame is then the last, as the verifier requires), or null
 * on the frame cap, `kill()`, or skip().
 */
export class Recorder {
  constructor(engine, task, start, won, onStatus = () => {}) {
    this.engine = engine;
    this.task = task;
    this.start = start;
    this.won = won;
    this.onStatus = onStatus;
    this.limit = task.sm64_max_frames;
    this.pads = []; // the pad played on every frame (pokes included)
    this.poked = []; // whether each frame was poked (proofs leave those out)
    this.speed = 1;
    this.paused = false;
    this.mode = 'play'; // 'play' | 'catchup'
    this.replayIdx = 0;
    this.replayTarget = 0;
    this.skipped = false;
    this.lastStart = false;
    this.snaps = [];
    this.pert = null;
    this.reset();
  }

  // --- controls ---
  restart() { this.rewindTo(0); }
  back(seconds) { this.rewindTo(Math.max(0, this.frame() - Math.round(seconds * 30))); }
  setSpeed(s) { this.speed = s; }
  setLimit(n) { this.limit = n; }
  togglePause() { this.paused = !this.paused; this.status(); }
  skip() { this.skipped = true; }
  takeOver() {
    if (this.mode !== 'catchup') return;
    this.pads.length = this.replayIdx; this.poked.length = this.replayIdx;
    this.dropSnapsAfter(this.replayIdx);
    this.mode = 'play';
    this.status();
  }

  frame() { return this.mode === 'catchup' ? this.replayIdx : this.pads.length; }

  status() {
    this.onStatus({
      frame: this.frame(),
      recorded: this.pads.length,
      limit: this.limit,
      speed: this.speed,
      paused: this.paused,
      mode: this.mode,
      target: this.replayTarget,
    });
  }

  // --- save states ---
  // The attempt's frame-0 state: the start, with a fresh perturbation.
  reset() {
    if (this.pert) this.pert.free();
    this.pert = new Perturbation(this.task);
    restoreState(this.engine, this.pert, { ...this.start.state, pert: null });
    this.snaps = [{ frame: 0, state: { ...this.start.state, pert: this.pert.toBytes() } }];
    this.pads = [];
    this.poked = [];
    this.mode = 'play';
    this.replayIdx = 0;
    this.replayTarget = 0;
  }

  dropSnapsAfter(frame) {
    this.snaps = this.snaps.filter((s) => s.frame <= frame);
  }

  snapshot(frame) {
    const last = this.snaps[this.snaps.length - 1];
    if (last && last.frame >= frame) return;
    this.snaps.push({ frame, state: captureState(this.engine, this.pert, last.state) });
    while (this.snaps.length > MAX_SNAPS) this.snaps.splice(1, 1); // keep the start
  }

  rewindTo(k) {
    k = Math.min(k, this.pads.length);
    if (k === 0) { this.reset(); this.status(); return; }
    // Restore the latest state at or before k and replay the recorded pads.
    this.dropSnapsAfter(k);
    const s = this.snaps[this.snaps.length - 1];
    restoreState(this.engine, this.pert, s.state);
    this.pads.length = k; this.poked.length = k;
    this.replayIdx = s.frame;
    this.replayTarget = k;
    this.mode = k > s.frame ? 'catchup' : 'play';
    this.lastStart = true;
    this.status();
  }

  // One frame from the replay; false if it doesn't match (shouldn't happen).
  replayOne() {
    const i = this.replayIdx;
    // Replaying the recorded (played) pads: on poked frames the perturbation
    // plays its own pad again, which must be the one recorded.
    const [played, poked] = this.pert.step(this.engine, this.pads[i]);
    if (!played.equals(this.pads[i])) return false;
    this.poked[i] = poked;
    this.replayIdx++;
    if (this.replayIdx % SNAP_EVERY === 0) this.snapshot(this.replayIdx);
    return true;
  }

  // One live frame; returns 'won', 'cap' or null.
  playOne(c) {
    const [pad, poked] = this.pert.step(this.engine, c);
    this.pads.push(pad);
    this.poked.push(poked);
    const n = this.pads.length;
    if (this.won(this.start.progress, readProgress(this.engine))) return 'won';
    if (n % SNAP_EVERY === 0) this.snapshot(n);
    if (n >= MAX_RUN_FRAMES) return 'cap';
    return null;
  }

  async run(kill) {
    this.status();
    return new Promise((resolve) => {
      let last = performance.now();
      let owed = 0; // frames due at the current speed
      const finish = (v) => { this.engine.set_audio_enabled(0); this.status(); resolve(v); };
      const tick = async (now) => {
        if ((await kill()) || this.skipped) { finish(null); return; }
        const dt = Math.min(now - last, 250);
        last = now;
        if (this.paused) { requestAnimationFrame(tick); return; }

        if (this.mode === 'catchup') {
          const c = applyTouch(this.engine.get_controller_pad());
          const start = c.is_pressed(GamepadButtons.START_BUTTON);
          if (start && !this.lastStart) this.takeOver();
          this.lastStart = start;
          const fastUntil = this.replayTarget - CATCH_UP_TAIL;
          const fast = this.replayIdx < fastUntil;
          let n;
          if (fast) {
            n = Math.min(CATCH_UP_PER_TICK, fastUntil - this.replayIdx);
          } else {
            owed += (dt / 1000) * 30 * Math.min(this.speed, 1);
            n = Math.floor(owed);
            owed -= n;
          }
          this.engine.set_audio_enabled(fast ? 0 : 1);
          while (n-- > 0 && this.mode === 'catchup' && this.replayIdx < this.replayTarget) {
            if (!this.replayOne()) { this.pads.length = this.replayIdx; this.poked.length = this.replayIdx; break; }
          }
          if (this.mode === 'catchup' && this.replayIdx >= this.replayTarget) { this.mode = 'play'; this.lastStart = true; }
          this.status();
          requestAnimationFrame(tick);
          return;
        }

        // Live play. Hold D-pad down for 10x (the old miner's fast-forward).
        const c0 = applyTouch(this.engine.get_controller_pad());
        const ff = c0.is_pressed(GamepadButtons.D_JPAD);
        if (c0.is_pressed(GamepadButtons.U_JPAD)) { this.back(10); requestAnimationFrame(tick); return; }
        const speed = ff ? 10 : this.speed;
        this.engine.set_audio_enabled(speed === 1 ? 1 : 0);
        owed += (dt / 1000) * 30 * speed;
        let steps = Math.min(Math.floor(owed), 60);
        owed -= Math.floor(owed);
        while (steps-- > 0) {
          const c = applyTouch(this.engine.get_controller_pad());
          // The START that ended a catch-up must not reach the game.
          if (this.lastStart) {
            if (c.is_pressed(GamepadButtons.START_BUTTON)) c.disable_button(GamepadButtons.START_BUTTON);
            else this.lastStart = false;
          }
          const r = this.playOne(c);
          if (r === 'won') { finish({ pads: this.pads, poked: this.poked }); return; }
          if (r === 'cap') { finish(null); return; }
        }
        this.status(); // not every 10th frame: at fast speeds the count steps past multiples of 10
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    });
  }
}

// Replays a block's run on `engine` (already at the block's start state, with
// `startProgress`), checking every frame against the perturbation like the
// verifier does. `pads` are the pads played on every frame. Resolves to
// 'won', 'mismatch:<frame>', 'ended' or 'killed'.
export async function playRun(engine, task, startProgress, pads, won, kill, speed = () => 1, onFrame = () => {}) {
  const pert = new Perturbation(task);
  let i = 0;
  try {
    return await new Promise((resolve) => {
      let last = performance.now();
      let owed = 0;
      const tick = async (now) => {
        if (await kill()) { engine.set_audio_enabled(0); resolve('killed'); return; }
        const s = speed();
        engine.set_audio_enabled(s === 1 ? 1 : 0);
        owed += (Math.min(now - last, 250) / 1000) * 30 * s;
        last = now;
        let steps = Math.floor(owed);
        owed -= steps;
        while (steps-- > 0) {
          if (i >= pads.length) { engine.set_audio_enabled(0); resolve('ended'); return; }
          const [played] = pert.step(engine, pads[i]);
          if (!played.equals(pads[i])) { engine.set_audio_enabled(0); resolve(`mismatch:${i}`); return; }
          i++;
          if (won(startProgress, readProgress(engine))) { onFrame(i); engine.set_audio_enabled(0); resolve('won'); return; }
        }
        onFrame(i);
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    });
  } finally {
    pert.free();
  }
}

export { fastForward };
