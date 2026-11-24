// A web miner worker: one campaign (the native miner's rrt::mine) on one block template. The
// campaign runs in one WebAssembly instance; each of its steps runs in a fresh instance (see
// runtime.js). Messages in: {type: 'mine', request}; out: {type: 'log', line}, {type: 'result',
// result} or {type: 'error', message}. A new tip: the page terminates the worker.
import { makeMiner } from './runtime.js';
import { getFile } from 'sm64-binds-src/scripts/fileUpload.js';
import wasmUrl from '../../wasm/webminer/sm64coin_webminer_bg.wasm?url';
import glue from '../../wasm/webminer/sm64coin_webminer.js?raw';

self.onmessage = async (e) => {
  if (e.data?.type !== 'mine') return;
  try {
    const [module, rom] = await Promise.all([WebAssembly.compileStreaming(fetch(wasmUrl)), getFile('ROM')]);
    const m = makeMiner({ module, glue, rom: new Uint8Array(rom), log: (line) => self.postMessage({ type: 'log', line }) });
    const result = m.mine(e.data.request);
    self.postMessage({ type: 'result', result, steps: m.steps() });
  } catch (err) {
    self.postMessage({ type: 'error', message: String(err?.message || err) });
  }
};
