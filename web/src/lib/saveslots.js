// The manual miner's save-state slots (like an emulator's): pure bookkeeping, no engine here.
//
// A slot holds what the Recorder needs to put the attempt back exactly: the frame number, the FULL pad prefix played up
// to it (and which frames were poked), the game state at that frame (lib/savestate.js: memory + the perturbation's
// bytes) and the recorder's periodic saves up to it (so going back past the slot is still quick). Loading restores that
// prefix: the recording becomes the slot's recording, whether that is shorter or LONGER than the current one (a slot
// saved at frame 900, then a rewind to 300, then load: you are at 900 again with the pads you played then). The run
// stays an ordinary valid proof because the recording is always exactly the pads that led to the engine's state.
//
// Slots belong to one attempt's block template + start (the bank's `key`): a bank for another key is empty.

export const SLOT_COUNT = 4;

// Course numbers of the game state (courseNum): 0 is the castle and its grounds.
const COURSES = ['Castle', 'Bob-omb Battlefield', 'Whomp\'s Fortress', 'Jolly Roger Bay', 'Cool, Cool Mountain', 'Big Boo\'s Haunt',
  'Hazy Maze Cave', 'Lethal Lava Land', 'Shifting Sand Land', 'Dire, Dire Docks', 'Snowman\'s Land', 'Wet-Dry World', 'Tall Tall Mountain',
  'Tiny-Huge Island', 'Tick Tock Clock', 'Rainbow Ride', 'Bowser in the Dark World', 'Bowser in the Fire Sea', 'Bowser in the Sky',
  'Princess\'s Secret Slide', 'Cavern of the Metal Cap', 'Tower of the Wing Cap', 'Vanish Cap under the Moat', 'Wing Mario over the Rainbow',
  'Secret Aquarium'];
export const courseLabel = (n) => (n >= 0 && n < COURSES.length ? COURSES[n] : `Course ${n}`);

/** A slot from the recorder's live data; copies the pad prefix so later play can't change it. */
export function makeSlot({ frame, pads, poked, snaps, state, info = {} }) {
  if (pads.length < frame) throw new Error(`slot at frame ${frame} but only ${pads.length} pads`);
  return {
    frame,
    pads: pads.slice(0, frame),
    poked: poked.slice(0, frame),
    snaps: snaps.filter((s) => s.frame <= frame),
    state,
    info,
  };
}

export class SlotBank {
  constructor(key, count = SLOT_COUNT) { this.key = key; this.slots = new Array(count).fill(null); }
  get count() { return this.slots.length; }
  get(i) { return this.slots[i] || null; }
  put(i, slot) { this.slots[i] = slot; }
  clear() { this.slots.fill(null); }
  /** Binds the bank to a block template / start: a different key empties it. Returns whether it did. */
  rebind(key) {
    if (key === this.key) return false;
    this.key = key; this.clear();
    return true;
  }
}

/** The key slots are tied to: the template's seed (its mempool) and where the attempt starts. */
export const slotKey = (task, start) => `${task.sm64_seed_hash || task.sm64_seed || ''}|${start && start.kind || ''}`;

export const SAVE_KEYS = ['z', 'x', 'c', 'v'];
export const LOAD_KEYS = ['7', '8', '9', '0'];
