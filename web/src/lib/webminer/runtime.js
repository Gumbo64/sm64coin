// The web miner's runtime (shared by the browser worker and the Node test): the native
// miner's RRT campaign compiled to WebAssembly (pow/webminer), run the way the native miner
// runs it:
//  * the campaign (`mine`) runs in one instance;
//  * every campaign step runs in a FRESH instance of the same compiled module, as natively in a
//    fresh `rrtrun` process (the search reads its settings once per process);
//  * files (the ROM, the campaign's scratch: start states, roadmap, priors, campaign memory) are
//    one JavaScript map shared by all of them (`globalThis.sm64fs`).
// A fresh instance has to be made synchronously (the campaign waits for its step), but the
// wasm-bindgen glue keeps its instance in module-level variables. The glue is self-contained, so
// its source is turned into a factory function: each call is an independent copy.

import { bestCheckpoint, makeChannel, makeTreeSync, makeNodeRequests, makeHybridPoll } from './board.js';

function hexToBytes(h) {
  const b = new Uint8Array(h.length / 2);
  for (let i = 0; i < b.length; i++) b[i] = parseInt(h.substr(i * 2, 2), 16);
  return b;
}

/** A factory of independent instances from the wasm-bindgen glue's source text. */
export function makeFactory(glueSource) {
  const body = glueSource
    .replace(/^export \{[^}]*\};?\s*$/m, '')
    .replace(/^export (?=(class|function|const|let|async) )/gm, '')
    .replace(/import\.meta\.url/g, '"about:blank"');
  // eslint-disable-next-line no-new-func
  return new Function(`${body}\nreturn { init, mine, run_step, initSync };`);
}

/** The files: path -> Uint8Array. */
export function makeFs(map = new Map()) {
  const dirOf = (p) => (p.endsWith('/') ? p : `${p}/`);
  return {
    map,
    read: (p) => map.get(p),
    write: (p, bytes) => { map.set(p, new Uint8Array(bytes)); },
    remove: (p) => map.delete(p),
    list: (dir) => {
      const d = dirOf(dir);
      const names = new Set();
      for (const k of map.keys()) if (k.startsWith(d)) names.add(k.slice(d.length).split('/')[0]);
      return [...names].join('\n');
    },
  };
}

/**
 * Sets up the globals the module expects and returns {mine(request), instances}: `module` is the
 * compiled WebAssembly.Module, `glue` the glue's source text, `rom` the ROM bytes, `log(line)`
 * the campaign's progress.
 */
export function makeMiner({ module, glue, rom, log = () => {}, fs = makeFs(), shared = null, onShare = () => {}, board = null, onViz = null, viz = false, onState = () => {}, onPert = () => {}, onPath = () => {}, onEdges = () => {}, onHybrid = () => {} }) {
  const factory = makeFactory(glue);
  fs.write('/rom.z64', rom);
  globalThis.sm64fs = fs;
  let steps = 0;
  const chan = board ? makeChannel(board) : null;
  const treeSync = chan ? makeTreeSync(chan) : null;
  const nodeReq = board ? makeNodeRequests(board) : null;
  const hybridPoll = board ? makeHybridPoll(board) : null;
  globalThis.sm64miner = {
    log,
    // The step's start state and the inputs of nodes the page asked for (rrt::child::viz).
    viz_state: (key, bytes) => onState(key, bytes.slice()),
    viz_pert: (key, json) => onPert(key, json),
    viz_req: (key) => (nodeReq ? nodeReq(key) : undefined),
    viz_path: (key, id, flat) => onPath(key, id, flat.slice()),
    // The tree edges the page's mirror lacks (rrt::child::viz Edges): binary, copied out of the wasm memory.
    viz_edges: (key, bytes) => onEdges(key, bytes.slice()),
    // Sharing with the other workers of the page (rrt::Exchange): the block's first castle step
    // is made once; `shared` ({step, files}) is another worker's, `onShare` gets ours.
    // The tree sampler's feed (rrt::child::viz): `onViz(json line)`; on/off from the page's board flag
    // (live) or else `viz` (fixed at the worker's start).
    viz: (line) => onViz && onViz(line),
    viz_on: () => !!onViz && (board ? board.vizOn() : viz),
    exchange(kind, payload) {
      if (kind === 'step_get') {
        if (!shared || JSON.parse(shared.step).key !== payload) return undefined;
        for (const [p, bytes] of shared.files) fs.write(p, bytes);
        return shared.step;
      }
      if (kind === 'cp_sync') return chan ? bestCheckpoint(chan, payload) : undefined;
      // The search's own tree sync (rrt::child::tsync): needs the board (cross-origin isolated page);
      // without it the workers share only at step boundaries (step_get/step_put, cp_sync's absence).
      if (kind === 'tree_sync') return treeSync ? treeSync(payload) : undefined;
      // Hybrid mining (rrt::child::hybrid): the page's mailbox in, the answers (a node's state, an inserted node) out.
      if (kind === 'hybrid') return hybridPoll ? hybridPoll(payload) : undefined;
      if (kind === 'hybrid_out') {
        const m = JSON.parse(payload);
        onHybrid(m, typeof m.state === 'string' ? hexToBytes(m.state) : null);
        return undefined;
      }
      if (kind === 'step_put') {
        // What the step left in the scratch (the roadmap...); not the campaign's own memory.
        const files = [...fs.map].filter(([p]) => p.startsWith('/scratch/') && !p.includes('/step-') && !p.endsWith('/campaign.json'));
        onShare({ step: payload, files: files.map(([p, b]) => [p, new Uint8Array(b)]) });
      }
      return undefined;
    },
    run_child(job) {
      steps += 1;
      const child = factory();
      child.initSync({ module });
      // The step's instance is dropped afterwards (its memory goes with it), like an exited process.
      return child.run_step(job);
    },
  };
  const parent = factory();
  parent.initSync({ module });
  parent.init(rom);
  return {
    mine: (request, parentState) => JSON.parse(parent.mine(JSON.stringify(request), parentState || undefined)),
    steps: () => steps,
    fs,
  };
}
