// A web miner worker: one campaign (the native miner's rrt::mine) on one block template. The
// campaign runs in one WebAssembly instance; each of its steps runs in a fresh instance (see
// runtime.js). Messages in: {type: 'mine', request}; out: {type: 'log', line}, {type: 'result',
// result} or {type: 'error', message}, {type: 'share', ...} (the first castle step, see AutoMine) and,
// when the page asked ({viz: true} in the 'mine' message; {vizOn} its initial state, live switching through the
// board's flag when the page is cross-origin isolated), {type: 'progress', sim}: with the feed off, once a second the
// frames simulated so far in the current step (the page's frames/s readout), {type: 'vizstate', key, state}: the current step's start state (SaveState bytes), {type: 'vizpath', key, id, flat}: a node's inputs the page asked for, {type: 'vizedges', key, buf}: the search tree's edges the page's mirror lacks (binary, see child.rs `Edges`; hub.addEdges), {type: 'viz', line}: one JSON line about once a second
// of the running search tree, format documented at pow/miner/src/rrt/child.rs `viz`. A new tip: the page terminates the worker.
import { makeMiner } from './runtime.js';
import { makeBoard } from './board.js';
import { getFile } from 'sm64-binds-src/scripts/fileUpload.js';
import wasmUrl from '../../wasm/webminer/sm64coin_webminer_bg.wasm?url';
import glue from '../../wasm/webminer/sm64coin_webminer.js?raw';

self.onmessage = async (e) => {
  if (e.data?.type !== 'mine') return;
  try {
    const [module, rom] = await Promise.all([WebAssembly.compileStreaming(fetch(wasmUrl)), getFile('ROM')]);
    const m = makeMiner({ module, glue, rom: new Uint8Array(rom), log: (line) => self.postMessage({ type: 'log', line }),
      shared: e.data.shared || null,
      onViz: e.data.viz ? (line) => (line.startsWith('{"progress"') ? self.postMessage({ type: 'progress', sim: JSON.parse(line).sim_frames }) : self.postMessage({ type: 'viz', line })) : null,
      viz: !!e.data.vizOn,
      board: e.data.board ? makeBoard(e.data.board.sab, e.data.board.slot, e.data.board.slots) : null,
      onState: (key, bytes) => self.postMessage({ type: 'vizstate', key, state: bytes }, [bytes.buffer]),
      onEdges: (key, bytes) => self.postMessage({ type: 'vizedges', key, buf: bytes.buffer }, [bytes.buffer]),
      onPath: (key, id, flat) => self.postMessage({ type: 'vizpath', key, id, flat }, [flat.buffer]),
      onShare: (s) => self.postMessage({ type: 'share', ...s }, s.files.map(([, b]) => b.buffer)),
    });
    const result = m.mine({ ...e.data.request, viz: !!e.data.viz }, e.data.parentState ? new Uint8Array(e.data.parentState) : null);
    self.postMessage({ type: 'result', result, steps: m.steps() });
  } catch (err) {
    self.postMessage({ type: 'error', message: String(err?.message || err) });
  }
};
