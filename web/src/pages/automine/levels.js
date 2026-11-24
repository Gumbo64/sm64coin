// Level numbers (the game's LEVEL_* enum) -> names and icons, and the campaign's zone names.
export const LEVELS = {
  4: ['Big Boo\'s Haunt', '👻'], 5: ['Cool, Cool Mountain', '⛄'], 6: ['Inside the Castle', '🏰'], 7: ['Hazy Maze Cave', '💎'],
  8: ['Shifting Sand Land', '🏜️'], 9: ['Bob-omb Battlefield', '💣'], 10: ['Snowman\'s Land', '☃️'], 11: ['Wet-Dry World', '💧'],
  12: ['Jolly Roger Bay', '🐟'], 13: ['Tiny-Huge Island', '🍄'], 14: ['Tick Tock Clock', '⏰'], 15: ['Rainbow Ride', '🌈'],
  16: ['Castle Grounds', '🌳'], 17: ['Bowser in the Dark World', '🔥'], 18: ['Vanish Cap under the Moat', '🌊'],
  19: ['Bowser in the Fire Sea', '🌋'], 20: ['Secret Aquarium', '🐠'], 21: ['Bowser in the Sky', '☁️'], 22: ['Lethal Lava Land', '🌋'],
  23: ['Dire, Dire Docks', '⚓'], 24: ['Whomp\'s Fortress', '🗿'], 26: ['Castle Courtyard', '🏰'], 27: ['Peach\'s Secret Slide', '🛝'],
  28: ['Cavern of the Metal Cap', '🔩'], 29: ['Tower of the Wing Cap', '🪽'], 30: ['Bowser 1', '👹'], 31: ['Wing Mario over the Rainbow', '🪽'],
  33: ['Bowser 2', '👹'], 34: ['Bowser 3', '👹'], 36: ['Tall, Tall Mountain', '🐒'],
};
export const levelName = (l) => (LEVELS[l] ? LEVELS[l][0] : `Level ${l}`);
export const levelIcon = (l) => (LEVELS[l] ? LEVELS[l][1] : '❓');

/** A campaign step's name as the log prints it ("Bob", "Castle", "Wf"...) -> an icon. */
const BY_LOG = { Bob: 9, Wf: 24, Jrb: 12, Ccm: 5, Bbh: 4, Hmc: 7, Lll: 22, Ssl: 8, Ddd: 23, Sl: 10, Wdw: 11, Ttm: 36, Thi: 13, Ttc: 14, Rr: 15,
  Pss: 27, Sa: 20, Wmotr: 31, Totwc: 29, Cotmc: 28, Vcutm: 18, Bitdw: 17, Bitfs: 19, Bits: 21, Castle: 6, 'Castle Grounds': 16, 'Castle Courtyard': 26 };
export const logIcon = (name) => {
  const k = Object.keys(BY_LOG).find((x) => name.toLowerCase().startsWith(x.toLowerCase()));
  return k ? levelIcon(BY_LOG[k]) : '⭐';
};

/** Distinct, colour-blind-friendly series colours for workers. */
export const WORKER_COLORS = ['#f7c948', '#60a5fa', '#4ade80', '#f472b6', '#fb923c', '#a78bfa', '#2dd4bf', '#f87171'];
export const workerColor = (i) => WORKER_COLORS[i % WORKER_COLORS.length];
