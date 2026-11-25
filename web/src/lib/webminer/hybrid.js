// Hybrid mining, the page's side: the board's mailbox to the workers (a position goal, state requests, human-played
// segments) and their answers. The workers' searches run synchronously inside WebAssembly, so a request only reaches
// one through the shared board (see board.js postMail / makeHybridPoll) and is answered between two rounds of its search
// (rrt::child::hybrid); the answers come back as {type: 'hybrid'} worker messages (controller.js -> onMessage).
import { postMail } from './board.js';

/** How long a command stays in the mailbox (a step that comes later with the same tag still gets it). */
const KEEP_MS = 10 * 60 * 1000;

export class Hybrid {
  constructor() {
    this.board = null;
    this.slots = 0;
    this.cmds = [];
    this.goal = null; // {level, area, pos}
    this.nextId = 1;
    this.onLog = null; // (slot, text): a search's own note (the position goal's objectives)
    this.waiting = new Map(); // command id -> {resolve, reject, timer, onReply}
  }

  /** Whether commands can reach the workers (a round is running on a cross-origin-isolated page). */
  get available() { return !!this.board; }

  attach(board, slots) {
    this.board = board;
    this.slots = slots;
    this.cmds = [];
    for (const w of this.waiting.values()) { clearTimeout(w.timer); w.reject(new Error('a new round started')); }
    this.waiting.clear();
    this.post();
  }

  detach() {
    this.board = null;
    for (const w of this.waiting.values()) { clearTimeout(w.timer); w.reject(new Error('the miner stopped')); }
    this.waiting.clear();
  }

  post() {
    if (!this.board) return false;
    const now = Date.now();
    this.cmds = this.cmds.filter((c) => now - c.at < KEEP_MS);
    return postMail(this.board, this.slots, { goal: this.goal, cmds: this.cmds });
  }

  /** The position goal for the search's generator ({level, area, pos: [x,y,z]} or null). */
  setGoal(goal) {
    this.goal = goal;
    return this.post();
  }

  send(cmd, { timeout = 120000, onReply = null } = {}) {
    if (!this.board) return Promise.reject(new Error('hybrid mining needs the running auto-miner with worker sync (cross-origin isolation)'));
    const id = this.nextId++;
    this.cmds.push({ ...cmd, id, at: Date.now() });
    if (!this.post()) { this.cmds.pop(); return Promise.reject(new Error('the command does not fit the mailbox')); }
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.waiting.delete(id); reject(new Error('no answer from the search (it answers between its rounds; the step may have ended)')); }, timeout);
      this.waiting.set(id, { resolve, reject, timer, onReply });
    });
  }

  /** The state of node `nid` of worker `slot`'s tree (named `vkey` in the viz feed): {msg, state: Uint8Array}. */
  requestState(slot, nid, vkey, timeout) {
    return this.send({ kind: 'state', slot, nid, vkey }, { timeout });
  }

  /**
   * Adds the played segment to the trees of the workers on step `tag`: `path` (hex pads from the step's root to the node it was
   * played from), `seg` (hex pads played). Resolves with the first answer; `onReply(slot, msg)` sees every worker's.
   */
  insert({ tag, path, seg, weight = 8 }, onReply, timeout) {
    return this.send({ kind: 'insert', tag, path, seg, weight }, { timeout, onReply });
  }

  /** A worker's {type: 'hybrid'} answer. */
  onMessage(slot, msg, state) {
    if (msg.kind === 'log') { if (this.onLog) this.onLog(slot, msg.text); return; }
    const w = this.waiting.get(msg.id);
    if (!w) return;
    if (w.onReply) w.onReply(slot, msg);
    if (msg.kind === 'state') {
      clearTimeout(w.timer);
      this.waiting.delete(msg.id);
      if (msg.ok) w.resolve({ msg, state, slot }); else w.reject(new Error(msg.why || 'the search could not export the state'));
      return;
    }
    // An insert goes to every worker on the step: the first success resolves it, the others' answers still reach onReply.
    if (w.done) return;
    if (msg.ok) {
      w.done = true;
      clearTimeout(w.timer);
      w.timer = setTimeout(() => this.waiting.delete(msg.id), 60000);
      w.resolve({ msg, slot });
    } else if (!w.grace) {
      // Only failures so far: give the other workers a moment, then report this one.
      w.grace = setTimeout(() => { if (!w.done) { w.done = true; clearTimeout(w.timer); this.waiting.delete(msg.id); w.reject(new Error(msg.why || 'not added')); } }, 4000);
    }
  }
}

/** Flat hex of pads: 8 digits a pad (button u16, stick x, stick y), as rrt::child::tsync. */
export function encPads(pads) {
  let s = '';
  for (const p of pads) {
    const b = p.button & 0xffff;
    s += b.toString(16).padStart(4, '0') + (p.stick_x & 0xff).toString(16).padStart(2, '0') + (p.stick_y & 0xff).toString(16).padStart(2, '0');
  }
  return s;
}
