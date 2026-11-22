// Browser save states of the visual game engine (local, in memory).
//
// Kept behind this small interface so it can be swapped for the cross-
// compatible (headless <-> visual) save states of sm64-binds' JS package:
//
//   captureState(engine, pert, prev?) -> state
//   restoreState(engine, pert, state)
//
// What a state holds:
// - The game's memory: the engine runs in one WebAssembly.Memory (640 MB
//   reserved, ~15 MB used: static data and bss up to 0xc55ae4, then the C
//   stack, which ends at about 0xe54000; nothing lives above, and nothing the
//   game keeps is in the JS heap). We copy [0, top) in 4 KB pages; a page that
//   didn't change since `prev` is shared with it, so periodic snapshots cost
//   only what changed. `top` starts at 0xe54000 and is extended (in 1 MB
//   steps) when anything non-zero is found above it, so a game that grows
//   (malloc) still saves completely.
// - The perturbation (outside the game): its serialized bytes.
//
// What does NOT live in the memory, and how restore stays robust:
// - GL objects (textures, shaders, buffers) are in the emscripten glue's JS
//   tables; the game's memory holds only their integer ids. Restoring a state
//   into the same engine it was taken from keeps those ids valid. Since a
//   cached texture slot may since have been re-uploaded with another texture,
//   restore empties the texture cache's hash table (gfx_texture_cache in
//   gfx_pc.c: 1024 pointers, then the node pool that keeps its GL ids), so the
//   game re-uploads whatever it draws next and nothing shows a wrong texture.
//   The address is from the visual build's linker map (build/us_web/sm64.map,
//   like pow/perturb/src/sm64_addrs.rs); rebuild the game -> update TEX_CACHE.
//   Restoring into another engine instance isn't supported (its GL ids differ).
// - The keyboard: the game and SDL keep which keys are down in their memory
//   (keyboard_buttons_down, SDL_keyboard). Restoring those from the snapshot
//   would leave a key stuck down (or lose one held now), because the key-up
//   already happened. Restore keeps their live bytes. (A gamepad's SDL state
//   is not handled: SDL keeps joystick lists on the heap; a pad plugged in
//   between a snapshot and its restore needs a reconnect.)
// - Audio is a JS queue: it only affects what you hear, and restore silences
//   it (callers keep audio off while seeking).
// Game logic doesn't depend on any of that, so frames after a restore are
// identical to the original ones (verified by the e2e checks).

const PAGE = 4096;
const DEFAULT_TOP = 0xe54000;
const WINDOW = 1 << 20; // how far above `top` to look for growth
const TEX_CACHE = 0xc4b210; // gfx_texture_cache (visual build): hashmap[1024] pointers first
const TEX_HASHMAP_BYTES = 1024 * 4;
// Input state that must survive a restore: [address, length] (visual build map).
const KEEP_LIVE = [
  [0xc4f234, 4], // keyboard_buttons_down
  [0xc53f98, 0xc10], // SDL_keyboard
];

const tops = new WeakMap(); // engine -> memory top seen so far

function findTop(engine) {
  let top = tops.get(engine) ?? DEFAULT_TOP;
  const mem = engine.memory.buffer;
  for (;;) {
    const lo = top;
    const hi = Math.min(top + WINDOW, mem.byteLength);
    if (hi <= lo) break;
    const w = new Uint32Array(mem, lo, (hi - lo) >> 2);
    let last = -1;
    for (let i = w.length - 1; i >= 0; i--) if (w[i] !== 0) { last = i; break; }
    if (last < 0) break; // nothing above: done
    top = Math.min(Math.ceil((lo + (last + 1) * 4 + WINDOW) / PAGE) * PAGE, mem.byteLength);
  }
  tops.set(engine, top);
  return top;
}

function samePage(live, saved, off) {
  const a = new Uint32Array(live, off, PAGE >> 2);
  const b = new Uint32Array(saved.buffer, saved.byteOffset, PAGE >> 2);
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

/** Snapshots the engine (and `pert`, a lib/perturb.js Perturbation, or null). */
export function captureState(engine, pert = null, prev = null) {
  const top = findTop(engine);
  const n = Math.ceil(top / PAGE);
  const mem = engine.memory.buffer;
  const pages = new Array(n);
  for (let i = 0; i < n; i++) {
    const off = i * PAGE;
    const old = prev && prev.mem.pages[i];
    pages[i] = old && samePage(mem, old, off) ? old : new Uint8Array(mem.slice(off, off + PAGE));
  }
  return { mem: { pages, top }, pert: pert ? pert.toBytes() : null };
}

/** Puts the engine (and `pert`) back as they were at `state`. */
export function restoreState(engine, pert, state) {
  try { engine.set_audio_enabled(0); } catch { /* not created yet */ }
  const u8 = new Uint8Array(engine.memory.buffer);
  const live = KEEP_LIVE.map(([a, l]) => u8.slice(a, a + l));
  const pages = state.mem.pages;
  for (let i = 0; i < pages.length; i++) u8.set(pages[i], i * PAGE);
  tops.set(engine, Math.max(tops.get(engine) ?? 0, state.mem.top));
  // What was written above the snapshot's range since is stale: clear it.
  const top = tops.get(engine);
  if (top > state.mem.top) u8.fill(0, state.mem.top, top);
  KEEP_LIVE.forEach(([a], i) => u8.set(live[i], a));
  u8.fill(0, TEX_CACHE, TEX_CACHE + TEX_HASHMAP_BYTES); // see above
  if (pert && state.pert) pert.loadBytes(state.pert);
}

/** Approximate bytes a state holds on its own (pages shared with others count
 * once, if you pass the set). */
export function stateBytes(state, seen = new Set()) {
  let n = 0;
  for (const p of state.mem.pages) if (!seen.has(p)) { seen.add(p); n += p.length; }
  return n;
}
