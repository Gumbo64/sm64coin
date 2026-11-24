// Web Worker: turns a node's saved end state (get_sm64_state: raw sm64-binds SaveState bytes of
// the HEADLESS build) into a portable state the visual engine can load. A headless game is
// made, the state's stack and heap are written into its memory exactly where the native
// SM64Game::load_state puts them (stack at 0, heap at the end of the first data segment), and
// the game exports its portable state. Message in: {id, state: ArrayBuffer (SaveState bytes)};
// out: {id, portable: Uint8Array} or {id, error}.
import { Sm64HeadlessEngine } from 'sm64-binds-src/scripts/sm64/Sm64Headless.js';
import { wasmCodeId } from 'sm64-binds-src/scripts/sm64/portable.js';
import { getHeadlessWASM } from 'sm64-binds-src/scripts/headlessWasm.js';

const MAGIC = 'SM64SAVE';
const HEADER = 44;
const PAGE = 65536;

function leb(b, p, signed) {
  let v = 0n, shift = 0n, byte;
  do {
    byte = b[p.at++];
    v |= BigInt(byte & 0x7f) << shift;
    shift += 7n;
  } while (byte & 0x80);
  if (signed && byte & 0x40) v -= 1n << shift;
  return signed ? Number(BigInt.asUintN(32, v)) : Number(v);
}

/** [address, length] of the first data segment (the read-only data), as memory.rs MemoryLayout. */
function readOnlyRange(wasm) {
  const b = new Uint8Array(wasm);
  let at = 8;
  while (at < b.length) {
    const id = b[at];
    const p = { at: at + 1 };
    const size = leb(b, p, false);
    const end = p.at + size;
    if (id === 11) {
      const n = leb(b, p, false);
      if (n < 2) return [0, 0]; // fewer than two segments: all of memory is mutable
      if (leb(b, p, false) !== 0 || b[p.at++] !== 0x41) throw new Error('unsupported data segment');
      const address = leb(b, p, true);
      if (b[p.at++] !== 0x0b) throw new Error('unsupported data segment address');
      return [address, address + leb(b, p, false)];
    }
    at = end;
  }
  throw new Error('no data section');
}

let ready = null;
let engine = null;
let range = null;
let buildId = null;

async function setup() {
  const wasm = await getHeadlessWASM();
  buildId = await wasmCodeId(wasm);
  range = readOnlyRange(wasm);
  engine = await Sm64HeadlessEngine.create(wasm);
}

function portableFrom(bytes) {
  if (bytes.length < HEADER || String.fromCharCode(...bytes.subarray(0, 8)) !== MAGIC) throw new Error('not an sm64-binds save state');
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (dv.getUint32(8, true) !== 1) throw new Error(`save state format version ${dv.getUint32(8, true)}`);
  const id = Array.from(bytes.subarray(12, 32), (x) => x.toString(16).padStart(2, '0')).join('');
  if (id !== buildId) throw new Error(`the node's state is from build ${id}, this headless game is ${buildId}`);
  const stackLen = dv.getUint32(32, true);
  const heapLen = Number(dv.getBigUint64(36, true));
  if (HEADER + stackLen + heapLen !== bytes.length) throw new Error('save state length does not match its header');
  if (stackLen !== range[0]) throw new Error(`state stack ${stackLen} bytes, this game's is ${range[0]}`);
  engine.restart();
  const need = range[1] + heapLen;
  const have = engine.memory.buffer.byteLength;
  if (have < need) engine.memory.grow(Math.ceil((need - have) / PAGE));
  const mem = new Uint8Array(engine.memory.buffer);
  mem.set(bytes.subarray(HEADER, HEADER + stackLen), 0);
  mem.set(bytes.subarray(HEADER + stackLen), range[1]);
  return engine.export_portable_state();
}

self.onmessage = async (e) => {
  const { id, state } = e.data;
  try {
    ready ??= setup().catch((err) => { ready = null; throw err; }); // a failed setup is retried next time
    await ready;
    const portable = portableFrom(new Uint8Array(state));
    self.postMessage({ id, portable }, [portable.buffer]);
  } catch (err) {
    self.postMessage({ id, error: String(err?.message ?? err) });
  }
};
