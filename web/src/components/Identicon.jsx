import React from 'react';
import { identicon, identiconColor } from '../lib/identicon.js';

/** An address's identicon (inline SVG, 16-48 px); hover shows the full address. */
export default function Identicon({ address, size = 20 }) {
  const { cells, hue } = identicon(address);
  return (
    <svg className="identicon" width={size} height={size} viewBox="0 0 5 5" fill={identiconColor(hue)} role="img" aria-label="address picture"
      style={{ verticalAlign: 'middle', borderRadius: 3, background: 'rgba(255,255,255,.06)', flex: 'none' }}>
      <title>{address}</title>
      {cells.flatMap((row, y) => row.map((on, x) => (on ? <rect key={`${x}-${y}`} x={x} y={y} width="1" height="1" /> : null)))}
    </svg>
  );
}
