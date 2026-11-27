// The desktop app's native miner as a viz source: the same messages the WASM workers post (worker.js: log, viz, vizstate,
// vizpert, vizedges, vizpath, hybrid) arrive as `miner-viz` events {w, kind, key, data} (desktop/src-tauri/src/miner.rs, from
// pow/miner/src/rrt/vizio.rs) and go to the same hub / hybrid handlers the controller gives the workers' messages. The native
// miner is one campaign, so one worker (index 0). Requests go the other way through the platform's mailbox (nm.vizMail).
import { daemon } from '../rpc.js';
import { newMinerKey, templateParams } from '../minerkey.js';
import { hybrid } from './controller.js';

const b64 = (s) => { const bin = atob(s); const u = new Uint8Array(bin.length); for (let i = 0; i < bin.length; i++) u[i] = bin.charCodeAt(i); return u; };
const hexToBytes = (h) => { const b = new Uint8Array(h.length / 2); for (let i = 0; i < b.length; i++) b[i] = parseInt(h.substr(i * 2, 2), 16); return b; };
/** A campaign line as the hub expects it: the native miner indents rrt::mine's log by two spaces. */
const unindent = (line) => (/^ {2}step \d+:/.test(line) ? line.slice(2) : line);

/** Dispatches one native frame like controller.js's worker.onmessage does. `hub`: the VizHub. */
export function dispatchFrame(hub, f, hyb = hybrid) {
  const w = f.w || 0;
  switch (f.kind) {
    case 'viz': hub.ingest(w, f.data); break;
    case 'state': hub.setRoot(w, f.key, b64(f.data)); break;
    case 'pert': hub.setPert(w, f.key, f.data); break;
    case 'edges': hub.addEdges(w, f.key, b64(f.data).buffer); break;
    case 'path': { const m = JSON.parse(f.data); hub.setPath(w, f.key, m.id, Int32Array.from(m.flat)); break; }
    case 'hybrid_out': {
      const m = JSON.parse(f.data);
      const state = typeof m.state === 'string' ? hexToBytes(m.state) : null;
      delete m.state;
      hyb.onMessage(w, m, state);
      break;
    }
    default: break;
  }
}

export class NativeFeed {
  constructor(miner, nm) {
    this.miner = miner;
    this.hub = miner.hub;
    this.nm = nm;
    this.fps = 0;
    this.offs = [];
    this.ready = false;
    this.queue = []; // frames that came before the round's template (the first samples)
    this.dead = false;
    this.line = 'starting…';
  }

  start() {
    const { nm, hub } = this;
    hybrid.attachSender((mail) => { nm.vizMail(mail).catch(() => {}); return true; });
    this.offs.push(nm.subscribeViz((f) => this.onFrame(f)));
    this.offs.push(nm.subscribe((line) => this.onLine(line)));
    this.offs.push(nm.onState((s) => { if (s && s.running === false) this.stop(); }));
    hub.requester = (w, key, nid) => this.ask(w, key, nid);
    this.round(true);
    this.watch = setInterval(() => this.poll(), 5000);
  }

  stop() {
    if (this.dead) return;
    this.dead = true;
    clearInterval(this.watch);
    this.offs.forEach((f) => f());
    hybrid.detach();
    this.hub.roster = null;
    this.hub.requester = (w, key, nid) => this.miner.askNode(w, key, nid);
    this.miner.nativeStopped(this);
  }

  /** A new template (first, or a new tip): the hub's round, the same as the controller's. */
  async round(first = false) {
    const { miner, hub } = this;
    let t = null;
    try {
      const tip = (await daemon('get_info')).top_block_hash || '';
      const key = await newMinerKey();
      t = await daemon('get_block_template', templateParams(miner.address, key, {}));
      t.tip = tip || t.prev_hash;
      t.minerKey = key;
    } catch (e) {
      if (first && !this.dead) setTimeout(() => this.round(true), 5000);
      return;
    }
    if (this.dead) return;
    this.template = t;
    hub.newRound({ template: t, starts: ['parent'], parentRaw: null });
    miner.nativeRound(t);
    this.ready = true;
    for (const f of this.queue.splice(0)) dispatchFrame(hub, f);
  }

  async poll() {
    if (this.dead || !this.template) return;
    try {
      const i = await daemon('get_info');
      if (i.top_block_hash !== this.template.tip) this.round();
    } catch { /* node hiccup */ }
  }

  onFrame(f) {
    if (this.dead) return;
    if (!this.ready) { if (this.queue.length < 400) this.queue.push(f); return; }
    dispatchFrame(this.hub, f);
  }

  onLine(raw) {
    if (this.dead) return;
    if (raw.startsWith('progress ')) {
      const m = /fps=([\d.]+)/.exec(raw);
      if (m) { this.fps = +m[1]; this.miner.emit(); }
      return;
    }
    const line = unindent(raw);
    this.line = line.trim();
    // A new campaign (a new attempt on the template): its first step starts over in the strips.
    const w = this.hub.workers[0];
    if (/^step 1:/.test(line) && w && w.stepNo > 1) this.hub.restartWorker(0, 'parent');
    this.hub.log(0, line);
    this.miner.nativeLine(this.line);
  }

  setViz(on) { this.nm.vizOn(on).catch(() => {}); }

  /** Asks the search for the inputs of node `nid` of its step `key` (answered as a `path` frame at its next sample). */
  ask(w, key, nid) {
    this.reqs = [...(this.reqs || []).slice(-40), { id: (this.seq = (this.seq || 0) + 1), slot: w, key, nid }];
    this.nm.vizMail({ reqs: this.reqs }).catch(() => {});
    return true;
  }
}
