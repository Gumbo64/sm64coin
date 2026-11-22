import React, { useEffect, useState, useCallback } from 'react';

// Any leaderboard server can be used (anyone can run one: scripts/leaderboard.sh);
// '' means this site's own proxy.
const apiBase = () => localStorage.getItem('sm64coin.leaderboard') || '/leaderboard-api';
const fmt = (a) => (a / 1e12).toLocaleString(undefined, { maximumFractionDigits: 6 });

export default function Leaderboard() {
  const [rows, setRows] = useState(null);
  const [err, setErr] = useState('');
  const [health, setHealth] = useState(null);

  const load = useCallback(() => {
    fetch(apiBase() + '/leaderboard').then((r) => r.json()).then((j) => { setRows(j); setErr(''); })
      .catch((e) => setErr('Leaderboard service unreachable: ' + e.message));
    fetch(apiBase() + '/health').then((r) => r.json()).then(setHealth).catch(() => setHealth(null));
  }, []);
  useEffect(() => { load(); const t = setInterval(load, 15000); return () => clearInterval(t); }, [load]);

  const totalBlocks = (rows || []).reduce((s, r) => s + r.blocks, 0);

  return (
    <div>
      <div className="card">
        <h2>Leaderboard</h2>
        <ServerPicker onChange={load} />
        <div className="stats">
          <div className="stat"><div className="muted">Participants</div><div>{rows ? rows.length : '-'}</div></div>
          <div className="stat"><div className="muted">Blocks counted</div><div>{rows ? totalBlocks : '-'}</div></div>
          <div className="stat"><div className="muted">Peer servers</div><div>{health ? `${health.peers} of ${health.known_peers}` : '-'}</div></div>
        </div>
        <p className="muted">The top 50, shared between leaderboard servers (off-chain). This server checks every entry itself
          with the entry's view key against the blockchain; blocks and rewards are never taken from other servers.</p>
        {err && <p className="muted">{err}</p>}
        {rows && rows.length === 0 && <p className="muted">Nobody has joined yet.</p>}
        {rows && rows.length > 0 && (
          <table>
            <thead><tr><th>#</th><th>Name</th><th>Address</th><th>Blocks</th><th>Rewards (SM64)</th><th>Last block</th><th>Best frames</th><th>Source</th></tr></thead>
            <tbody>
              {rows.map((r, i) => (
                <tr key={r.address_short + i}>
                  <td>{i + 1}</td><td>{r.name}</td>
                  <td className="mono" title={r.address || r.address_short}>{r.address_short}{r.address && <CopyButton text={r.address} />}</td>
                  <td>{r.blocks}</td><td>{fmt(r.reward_atomic)}</td>
                  <td>{r.last_block_height ?? '-'}</td><td>{r.best_frames ?? '-'}</td>
                  <td className="muted" title="Every row is verified by this server's own chain scan">{r.verified ? '✓ ' : ''}{r.source === 'peer' ? 'from peers' : r.source === 'local' ? 'joined here' : ''}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      <div className="card">
        <h2>Join the leaderboard</h2>
        <p>
          Joining is opt-in and done from the <b>SM64Coin wallet</b> app: <b>Advanced → Leaderboard</b>. It shares your
          address and private view key with your leaderboard server, so it can count the blocks your wallet received.
          If you rank in the top 50, your entry is shared with the other leaderboard servers (not on the blockchain), so everyone who
          runs one can see all of your incoming payments (a view key can't spend). Use a dedicated mining address.
        </p>
        <p className="muted">This website never asks for your keys. The sm64coin launcher runs a leaderboard server by default
          (port 16490); to use another one, point this page at it above.</p>
      </div>
    </div>
  );
}

function ServerPicker({ onChange }) {
  const [v, setV] = React.useState(localStorage.getItem('sm64coin.leaderboard') || '');
  const save = () => { if (v) localStorage.setItem('sm64coin.leaderboard', v.replace(/\/$/, '')); else localStorage.removeItem('sm64coin.leaderboard'); onChange(); };
  return (
    <p className="muted">Leaderboard server:{' '}
      <input className="mono" style={{ width: 320 }} value={v} placeholder="this site (default) or http://host:16490" onChange={(e) => setV(e.target.value.trim())} />{' '}
      <button onClick={save}>Use</button>
    </p>
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
