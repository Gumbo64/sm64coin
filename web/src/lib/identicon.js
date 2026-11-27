// GitHub-style identicons: a 5x5 left-right-symmetric pixel grid and one colour, both derived from keccak256 of the full address
// string (the same hash everywhere, nothing uploaded, no external service). Pure: identicon.test.mjs has fixed vectors.
import { keccak256 } from './block.js';

const enc = new TextEncoder();

/** {cells: boolean[5][5] (row, col), hue: 0-359} for an address string. */
export function identicon(address) {
  const h = keccak256(enc.encode(String(address || '')));
  const cells = [];
  for (let r = 0; r < 5; r++) {
    const row = new Array(5).fill(false);
    for (let c = 0; c < 3; c++) {
      const on = ((h[(r * 3 + c) >> 3] >> ((r * 3 + c) & 7)) & 1) === 1; // 15 bits: 3 columns, mirrored
      row[c] = on; row[4 - c] = on;
    }
    cells.push(row);
  }
  return { cells, hue: ((h[2] << 8) | h[3]) % 360 };
}

/** The colour: a fixed saturation and lightness that read on the dark theme. */
export const identiconColor = (hue) => `hsl(${hue} 55% 62%)`;

/** The picture as an SVG string (viewBox 0 0 5 5): the component and the tests share it. */
export function identiconSvg(address) {
  const { cells, hue } = identicon(address);
  const rects = cells.flatMap((row, y) => row.map((on, x) => (on ? `<rect x="${x}" y="${y}" width="1" height="1"/>` : ''))).join('');
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 5 5" fill="${identiconColor(hue)}">${rects}</svg>`;
}

/** "SM64 Yvab cdef … wxyz JPo1": the address's start and end in groups of four (monospace). */
export function groupedShort(a) {
  const s = String(a || '');
  if (s.length < 24) return s;
  const g = (x) => x.match(/.{1,4}/g).join(' ');
  return `${g(s.slice(0, 12))} … ${g(s.slice(-8))}`;
}
