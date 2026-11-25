// Group mining in the browser: trusted members mine ONE block template together, over iroh-gossip (the wasm build of
// pow/group, pow/groupweb; a browser endpoint reaches the others through n0's public relays, so nothing runs on the
// SM64Coin site). The leader fetches the template from its node and announces it (signed with its iroh key; members
// ignore templates from anyone else); the members share the first castle step and the runs they find. Whoever finds the
// winning run submits it, the reward goes to the leader's address (it is in the template's coinbase). The team code is
// a secret: anyone who has it joins. The controller (controller.js) calls this at four points: the template (round),
// the shared step (share), a found run (found) and the watcher (template/run changes).
const glob = import.meta.glob('../../wasm/groupweb/sm64coin_groupweb.js');
const wasmGlob = import.meta.glob('../../wasm/groupweb/sm64coin_groupweb_bg.wasm', { query: '?url', import: 'default' });
const ls = (k, d) => { try { return localStorage.getItem(k) ?? d; } catch { return d; } };
const lsSet = (k, v) => { try { localStorage.setItem(k, v); } catch { /* private window */ } };

/** Whether this build ships the group wasm (scripts/build-wasm.sh builds it; the site works without it). */
export const groupAvailable = Object.keys(glob).length > 0;

class GroupLink {
  constructor() {
    this.g = null;
    this.subs = new Set();
    this.name = ls('sm64coin.groupName', '');
    this.relay = ls('sm64coin.groupRelay', '');
    this.status = 'off'; // off | starting | on | error
    this.error = '';
    this.code = '';
    this.leader = false;
    this.info = { members: [], neighbors: 0, log: [] };
    this.logGen = 0;
    this.sgen = 0;
    this.seen = { tgen: 0, rgen: 0 };
    this.snap = this.makeSnap();
    this.handlers = {}; // {step(key, json), run(run), template()}
    this.timer = null;
  }
  subscribe = (fn) => { this.subs.add(fn); return () => this.subs.delete(fn); };
  getSnapshot = () => this.snap;
  makeSnap() {
    return { status: this.status, error: this.error, code: this.code, leader: this.leader, members: this.info.members, neighbors: this.info.neighbors, log: this.info.log, name: this.name, relay: this.relay, id: this.info.id || '' };
  }
  emit() { this.snap = this.makeSnap(); this.subs.forEach((f) => f()); }
  get active() { return this.status === 'on'; }
  get member() { return this.status === 'on' && !this.leader; }
  setName(n) { this.name = n.slice(0, 40); lsSet('sm64coin.groupName', this.name); if (this.g) this.g.set_name(this.name); this.emit(); }
  setRelay(r) { this.relay = r.trim(); lsSet('sm64coin.groupRelay', this.relay); this.emit(); }

  async load() {
    const [[loader], [wasm]] = [Object.values(glob), Object.values(wasmGlob)];
    if (!loader) throw new Error('this build has no group-mining module (scripts/build-wasm.sh groupweb)');
    const mod = await loader();
    await mod.default({ module_or_path: await wasm() });
    return mod;
  }
  async open(make) {
    if (this.status === 'starting' || this.status === 'on') return;
    this.status = 'starting'; this.error = ''; this.emit();
    try {
      const mod = await this.load();
      this.g = await make(mod.WebGroup);
      this.code = this.g.code();
      this.leader = this.g.is_leader();
      this.status = 'on';
      this.logGen = 0; this.sgen = 0; this.seen = { tgen: 0, rgen: 0 };
      this.timer = setInterval(() => this.poll(), 1000);
      this.poll();
    } catch (e) {
      this.status = 'error'; this.error = String(e?.message || e); this.g = null;
    }
    this.emit();
  }
  create() { return this.open((G) => G.create(this.name || 'browser', this.relay || undefined)); }
  join(code) { return this.open((G) => G.join(code.trim(), this.name || 'browser', this.relay || undefined)); }
  leave() {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
    try { if (this.g) { this.g.leave(); } } catch { /* already gone */ }
    // Leave the endpoint a moment to say goodbye before it is dropped.
    const g = this.g;
    setTimeout(() => { try { g && g.free(); } catch { /* done */ } }, 1500);
    this.g = null; this.status = 'off'; this.code = ''; this.leader = false; this.error = '';
    this.info = { members: [], neighbors: 0, log: [] };
    this.emit();
    if (this.handlers.left) this.handlers.left();
  }

  poll() {
    const g = this.g;
    if (!g) return;
    try {
      const s = JSON.parse(g.state(this.logGen));
      this.logGen = s.loggen;
      this.info = { ...s, log: [...(this.info.log || []), ...s.log].slice(-60) };
      const ns = JSON.parse(g.new_steps(this.sgen));
      this.sgen = ns.sgen;
      for (const st of ns.steps) this.handlers.step && this.handlers.step(st.key, st.json);
      if (s.tgen !== this.seen.tgen) { this.seen.tgen = s.tgen; this.handlers.template && this.handlers.template(); }
      if (s.rgen !== this.seen.rgen) { this.seen.rgen = s.rgen; this.handlers.run && this.handlers.run(); }
    } catch (e) { this.error = String(e); }
    this.emit();
  }

  allSteps() { return this.g ? JSON.parse(this.g.new_steps(0)).steps : []; }
  /** Our search speed for the others' member list. */
  setFps(f) { if (this.g) this.g.set_fps(f); }
  /** The current template (the leader's): {tid, tgen, json (the node's template, parsed)} or null. */
  template() {
    if (!this.g) return null;
    const t = this.g.template();
    if (!t) return null;
    const o = JSON.parse(t);
    return { tid: o.tid, tgen: o.tgen, t: JSON.parse(o.json) };
  }
  /** Leader: announce the node's template (with our address in it, for the members' valid-at queries). */
  publish(t, address) { if (this.g && this.leader) this.g.publish_template(JSON.stringify({ ...t, group_address: address })); }
  putStep(stepJson) {
    try { const k = JSON.parse(stepJson).key; if (this.g && k) this.g.put_step(k, stepJson); } catch { /* not a step */ }
  }
  putRun(start, frames, proofHex, keys = []) { if (this.g) this.g.put_run(start, frames, proofHex, JSON.stringify(keys)); }
  /** The best run another member shared for the start kind: {frames, proof, start, from} or null (ours are skipped). */
  bestRun(start) {
    if (!this.g) return null;
    const r = this.g.best_run(start);
    return r ? JSON.parse(r) : null;
  }
}

export const groupLink = new GroupLink();
window.__sm64group = groupLink; // for the e2e scripts
