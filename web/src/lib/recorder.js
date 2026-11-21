import { Sm64VisualEngine, GamepadButtons } from 'sm64-binds-frontend';
import { Seed } from './seed.js';

async function newEngine(canvas, task) {
  const seed = new Seed(task.sm64_seed_hash);
  canvas.id = 'canvas'; // the emscripten build looks the canvas up by id
  const engine = await Sm64VisualEngine.create(canvas, seed.word(0), task.config.random_config);
  canvas.id = '';
  return { engine, seed };
}

// Frames replayed per animation frame while catching up after a rewind.
const CATCH_UP_PER_TICK = 300;
// The last this many frames of a catch-up play at normal speed, so you see
// where you'll take over.
const CATCH_UP_TAIL = 90;

/**
 * A mining attempt in the browser, with the old miner's controls.
 *
 * Every frame the player's pad goes through the perturbation
 * (engine.rng_pad) and the pad actually played is recorded. The game can't
 * be saved and restored, but it is deterministic, so going back means a new
 * engine replaying the recorded pads up to that point (fast, then the last
 * 3 s at normal speed), after which you play on. During a catch-up, "take
 * over" (or Start) cuts the run there.
 *
 * `run(kill)` resolves with the pads when `won(state)` first holds (the
 * star is then the last frame, as the verifier requires), or null on the time
 * limit, `kill()`, or skip().
 */
export class Recorder {
  constructor(canvas, task, won, onStatus = () => {}) {
    this.canvas = canvas;
    this.task = task;
    this.won = won;
    this.onStatus = onStatus;
    this.max = task.config.max_solution_time;
    this.pads = [];
    this.speed = 1;
    this.paused = false;
    this.mode = 'play'; // 'play' | 'catchup'
    this.replayIdx = 0;
    this.replayTarget = 0;
    this.pending = null; // engine being created
    this.skipped = false;
    this.lastStart = false;
  }

  // --- controls ---
  restart() { this.rewindTo(0); }
  back(seconds) { this.rewindTo(Math.max(0, this.frame() - Math.round(seconds * 30))); }
  setSpeed(s) { this.speed = s; }
  togglePause() { this.paused = !this.paused; this.status(); }
  skip() { this.skipped = true; }
  takeOver() {
    if (this.mode !== 'catchup') return;
    this.pads.length = this.replayIdx;
    this.mode = 'play';
    this.status();
  }

  frame() { return this.mode === 'catchup' ? this.replayIdx : this.pads.length; }

  status() {
    this.onStatus({
      frame: this.frame(),
      recorded: this.pads.length,
      max: this.max,
      speed: this.speed,
      paused: this.paused,
      mode: this.mode,
      target: this.replayTarget,
    });
  }

  rewindTo(k) {
    if (this.pending) return; // a rewind is already building its engine (two at once fight over the canvas id)
    // Keep the pads up to k; replay them on a fresh engine.
    const keep = this.pads.slice(0, Math.min(k, this.pads.length));
    this.pending = newEngine(this.canvas, this.task).then((e) => {
      if (this.engine) this.engine.set_audio_enabled(0);
      this.engine = e.engine;
      this.seed = e.seed;
      this.pads = keep;
      this.replayIdx = 0;
      this.replayTarget = keep.length;
      this.mode = keep.length > 0 ? 'catchup' : 'play';
      this.pending = null;
      this.status();
    });
  }

  // One frame from the replay; false if it doesn't match (shouldn't happen).
  replayOne() {
    const i = this.replayIdx;
    this.seed.reseed(this.engine, this.task.sm64_reseed_frames, i);
    const allowed = this.engine.rng_pad(this.pads[i]);
    if (!allowed.equals(this.pads[i])) return false;
    this.engine.step_game(this.pads[i]);
    this.replayIdx++;
    return true;
  }

  // One live frame; returns 'won', 'limit' or null.
  playOne(c) {
    this.seed.reseed(this.engine, this.task.sm64_reseed_frames, this.pads.length);
    const pad = this.engine.rng_pad(c);
    this.engine.step_game(pad);
    this.pads.push(pad);
    if (this.won(this.engine.get_game_state())) return 'won';
    if (this.pads.length >= this.max) return 'limit';
    return null;
  }

  async run(kill) {
    const e = await newEngine(this.canvas, this.task);
    this.engine = e.engine;
    this.seed = e.seed;
    this.status();
    return new Promise((resolve) => {
      let last = performance.now();
      let owed = 0; // frames due at the current speed
      const finish = (v) => { this.engine.set_audio_enabled(0); this.status(); resolve(v); };
      const tick = async (now) => {
        if ((await kill()) || this.skipped) { finish(null); return; }
        const dt = Math.min(now - last, 250);
        last = now;
        if (this.pending || this.paused) { requestAnimationFrame(tick); return; }

        if (this.mode === 'catchup') {
          const c = this.engine.get_controller_pad();
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
            if (!this.replayOne()) { this.pads.length = this.replayIdx; break; }
          }
          if (this.replayIdx >= this.replayTarget) { this.mode = 'play'; this.lastStart = true; }
          this.status();
          requestAnimationFrame(tick);
          return;
        }

        // Live play. Hold D-pad down for 10x (the old miner's fast-forward).
        const c0 = this.engine.get_controller_pad();
        const ff = c0.is_pressed(GamepadButtons.D_JPAD);
        if (c0.is_pressed(GamepadButtons.U_JPAD)) { this.back(10); requestAnimationFrame(tick); return; }
        const speed = ff ? 10 : this.speed;
        this.engine.set_audio_enabled(speed === 1 ? 1 : 0);
        owed += (dt / 1000) * 30 * speed;
        let steps = Math.min(Math.floor(owed), 60);
        owed -= Math.floor(owed);
        while (steps-- > 0) {
          const c = this.engine.get_controller_pad();
          // The START that ended a catch-up must not reach the game.
          if (this.lastStart) {
            if (c.is_pressed(GamepadButtons.START_BUTTON)) c.disable_button(GamepadButtons.START_BUTTON);
            else this.lastStart = false;
          }
          const r = this.playOne(c);
          if (r === 'won') { finish(this.pads); return; }
          if (r === 'limit') { finish(null); return; }
        }
        this.status(); // not every 10th frame: at fast speeds the count steps past multiples of 10
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    });
  }
}

// One mining attempt without the controls (kept for simple callers).
export async function recordRun(canvas, task, won, kill, onFrame = () => {}, pads = []) {
  const r = new Recorder(canvas, task, won, (s) => onFrame(s.frame));
  const done = await r.run(kill);
  pads.length = 0;
  pads.push(...r.pads);
  return done;
}

// Replays a block's proof, checking every frame against the perturbation
// like the verifier does. Resolves to 'won', 'mismatch:<frame>', 'ended' or
// 'killed'.
export async function playRun(canvas, task, pads, won, kill, speed = () => 1, onFrame = () => {}) {
  const { engine, seed } = await newEngine(canvas, task);
  let i = 0;
  return new Promise((resolve) => {
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
        seed.reseed(engine, task.sm64_reseed_frames, i);
        if (!engine.rng_pad(pads[i]).equals(pads[i])) { engine.set_audio_enabled(0); resolve(`mismatch:${i}`); return; }
        engine.step_game(pads[i]);
        i++;
        if (won(engine.get_game_state())) { onFrame(i); engine.set_audio_enabled(0); resolve('won'); return; }
      }
      onFrame(i);
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}
