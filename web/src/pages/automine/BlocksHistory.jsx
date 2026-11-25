import React from 'react';
import { Link } from 'react-router';
import { useHub } from './useHub.js';
import { frames as fmtFrames } from '../../lib/format.js';

const LABEL = { accepted: '★ accepted', rejected: 'rejected', stale: 'too late (a new block landed)' };

// The blocks this page submitted in this session, with a link to each one's replay.
export default function BlocksHistory({ hub }) {
  const v = useHub(hub);
  void v;
  return (
    <div className="viz-card">
      <h3>Blocks mined <span className="muted small">this session</span></h3>
      {hub.blocks.length === 0 ? <p className="muted small">None yet: a run is submitted as soon as a worker finds one.</p> : (
        <table className="small viz-blocks"><tbody>
          {hub.blocks.map((b, i) => (
            <tr key={i} className={b.status}>
              <td>{new Date(b.at).toLocaleTimeString()}</td>
              <td>{b.height != null ? (b.status === 'accepted' ? <Link to={`/block/${b.height}`}>block {b.height}</Link> : `block ${b.height}`) : ''}</td>
              <td>{b.frames} fr ({fmtFrames(b.frames || 0)})</td>
              <td title={b.note ? String(b.note).slice(0, 200) : ''}>{LABEL[b.status] || b.status}</td>
            </tr>
          ))}
        </tbody></table>
      )}
    </div>
  );
}
