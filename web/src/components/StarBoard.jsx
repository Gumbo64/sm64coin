import React from 'react';
import { useStored } from './ui.jsx';

/**
 * Which of the 120 stars the block's start state has (★) and which are left (☆), one row per course, with the cap switches and
 * Bowser keys as a line under it. `board` is the Rust reader's answer (pow/perturb/src/stars.rs through perturb-wasm's `star_board`,
 * lib/perturb.js `readStarBoard`): { got, total, groups: [{ name, short, stars: [{ name, got }] }], switches: [{ name, got }] }.
 * Design from the sm64-viewer's replay picker.
 */
export default function StarBoard({ board }) {
  const [open, setOpen] = useStored('sm64coin.starBoardOpen', true);
  if (!board || !Array.isArray(board.groups)) return null;
  const left = board.total - board.got;
  return (
    <section className="card ui-side star-board" data-testid="star-board">
      <button type="button" className="star-board-head" onClick={() => setOpen(!open)} aria-expanded={open}>
        <b>{board.got}</b> of {board.total} stars · {left} left <span className="tri">{open ? '▾' : '▸'}</span>
      </button>
      {open && (
        <>
          <div className="star-grid" role="table" aria-label="Stars collected and left">
            {board.groups.map((g) => (
              <div className="star-row" role="row" key={g.short}>
                <span className="star-course" role="rowheader" title={g.name}>{g.short}</span>
                <span className="star-cells" role="cell">
                  {g.stars.map((x, i) => (
                    <span key={i} className={`star-cell${x.got ? ' got' : ''}`} title={`${x.name}: ${x.got ? 'collected' : 'not yet'}`} aria-label={`${x.name}: ${x.got ? 'collected' : 'not yet'}`}>{x.got ? '★' : '☆'}</span>
                  ))}
                </span>
                <span className="star-count muted small" role="cell">{g.stars.filter((x) => x.got).length}/{g.stars.length}</span>
              </div>
            ))}
          </div>
          {board.switches && board.switches.length > 0 && (
            <p className="small star-switches">
              {board.switches.map((w) => <span key={w.name} className={w.got ? 'got' : 'muted'}>{w.got ? '■' : '□'} {w.name}</span>)}
            </p>
          )}
        </>
      )}
    </section>
  );
}
