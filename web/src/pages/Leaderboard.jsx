import React, { useEffect, useState, useCallback } from 'react';
import Identicon from '../components/Identicon.jsx';
import { daemon, NODE_HINT } from '../lib/rpc.js';

// The board every node keeps and shares with its peers over p2p (the node's
// sm64_board RPC): the top 50 addresses by blocks mined, each counted by the
// node itself with the entry's view key.
const BoardJoin = React.lazy(() => import('./BoardJoin.jsx'));
const fmt = (a) => (a / 1e12).toLocaleString(undefined, { maximumFractionDigits: 6 });
import { groupedShort } from '../lib/identicon.js';

export default function Leaderboard() {
  const [rows, setRows] = useState(null);
  const [err, setErr] = useState('');
  const [off, setOff] = useState(false);

  const load = useCallback(() => {
    daemon('sm64_board').then((r) => { setRows(r.entries || []); setOff(!!r.disabled); setErr(''); })
      .catch((e) => setErr(`Node unreachable: ${e.message}. ${NODE_HINT}`));
  }, []);
  useEffect(() => { load(); const t = setInterval(load, 15000); return () => clearInterval(t); }, [load]);

  const totalBlocks = (rows || []).reduce((s, r) => s + r.blocks, 0);

  return (
    <div>
      <div className="card">
        <h2>Leaderboard</h2>
        <div className="stats">
          <div className="stat"><div className="muted">Miners</div><div>{rows ? rows.length : '-'}</div></div>
          <div className="stat"><div className="muted">Blocks counted</div><div>{rows ? totalBlocks : '-'}</div></div>
        </div>
        <p className="muted">The top 50 miners by blocks mined. Every node keeps this board and shares it with its peers;
          each node counts the blocks itself from the blockchain, with each entry's view key.</p>
        {err && <p className="muted">{err}</p>}
        {off && <p className="muted">The leaderboard is turned off on this node.</p>}
        {rows && !off && rows.length === 0 && <p className="muted">Nobody has joined yet.</p>}
        {rows && rows.length > 0 && (
          <table>
            <thead><tr><th>#</th><th>Name</th><th>Address</th><th>Blocks</th><th>Rewards (SM64)</th><th>Last block</th></tr></thead>
            <tbody>
              {rows.map((r, i) => (
                <tr key={r.address}>
                  <td>{i + 1}</td><td><Identicon address={r.address} size={24} /> {r.name}</td>
                  <td className="mono" title={r.address}>{groupedShort(r.address)}<CopyButton text={r.address} /></td>
                  <td>{r.blocks}</td><td>{fmt(r.reward)}</td><td>{r.last_height || '-'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      <React.Suspense fallback={null}><BoardJoin onJoined={load} /></React.Suspense>
    </div>
  );
}

// Copies `text`; navigator.clipboard needs a secure context (https or
// localhost), so fall back to a hidden textarea elsewhere (e.g. a LAN address).
function copyText(text) {
  if (navigator.clipboard && window.isSecureContext) return navigator.clipboard.writeText(text);
  const t = document.createElement('textarea');
  t.value = text;
  t.style.position = 'fixed';
  t.style.opacity = '0';
  document.body.appendChild(t);
  t.select();
  const ok = document.execCommand('copy');
  document.body.removeChild(t);
  return ok ? Promise.resolve() : Promise.reject(new Error('copy failed'));
}

function CopyButton({ text }) {
  const [done, setDone] = useState('');
  const copy = () => copyText(text).then(() => setDone('✓'), () => setDone('✗')).finally(() => setTimeout(() => setDone(''), 1500));
  return <button className="copy" title="Copy the full address" onClick={copy}>{done || '⧉'}</button>;
}
