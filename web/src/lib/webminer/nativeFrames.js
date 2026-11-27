// The native miner's `miner-viz` frames {w, kind, key, data} (desktop/src-tauri/src/miner.rs, from pow/miner/src/rrt/vizio.rs) turned
// into the calls the controller makes for the WASM workers' messages (worker.js): kind viz -> hub.ingest, state -> setRoot, pert ->
// setPert, edges -> addEdges, path -> setPath, hybrid_out -> hybrid.onMessage. Bytes arrive base64. No browser APIs here (tests run in node).
const atobBytes = (s) => { const bin = atob(s); const u = new Uint8Array(bin.length); for (let i = 0; i < bin.length; i++) u[i] = bin.charCodeAt(i); return u; };
export const b64decode = atobBytes;
const hexToBytes = (h) => { const b = new Uint8Array(h.length / 2); for (let i = 0; i < b.length; i++) b[i] = parseInt(h.substr(i * 2, 2), 16); return b; };
/** A campaign line as the hub expects it: the native miner indents rrt::mine's log by two spaces. */
export const unindent = (line) => (/^ {2}step \d+:/.test(line) ? line.slice(2) : line);

/** Dispatches one native frame like controller.js's worker.onmessage does. `hub`: the VizHub. */
export function dispatchFrame(hub, f, hyb) {
  const w = f.w || 0;
  switch (f.kind) {
    case 'viz': hub.ingest(w, f.data); break;
    case 'state': hub.setRoot(w, f.key, b64decode(f.data)); break;
    case 'pert': hub.setPert(w, f.key, f.data); break;
    case 'edges': hub.addEdges(w, f.key, b64decode(f.data).buffer); break;
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

