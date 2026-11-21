import React, { useEffect, useState, useCallback } from 'react';

// Any leaderboard server can be used (anyone can run one: scripts/leaderboard.sh);
// '' means this site's own proxy.
const apiBase = () => localStorage.getItem('sm64coin.leaderboard') || '/leaderboard-api';
const fmt = (a) => (a / 1e12).toLocaleString(undefined, { maximumFractionDigits: 6 });

async function walletRpc(method, params) {
  const r = await fetch('/wallet/json_rpc', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: '0', method, params }),
  });
  const j = await r.json();
  if (j.error) throw new Error(j.error.message);
  return j.result;
}

async function post(path, body) {
  const r = await fetch(apiBase() + path, { method: 'POST', body: JSON.stringify(body) });
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error || `HTTP ${r.status}`);
  return j;
}

export default function Leaderboard() {
  const [rows, setRows] = useState(null);
  const [err, setErr] = useState('');
  const [form, setForm] = useState({ name: '', address: '', view_key: '' });
  const [msg, setMsg] = useState('');
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    fetch(apiBase() + '/leaderboard').then((r) => r.json()).then((j) => { setRows(j); setErr(''); })
      .catch((e) => setErr('Leaderboard service unreachable: ' + e.message));
  }, []);
  useEffect(() => { load(); const t = setInterval(load, 15000); return () => clearInterval(t); }, [load]);

  const set = (k) => (e) => setForm({ ...form, [k]: e.target.value });

  const useWallet = async () => {
    setMsg('');
    try {
      const a = await walletRpc('get_address', { account_index: 0 });
      const k = await walletRpc('query_key', { key_type: 'view_key' });
      setForm((f) => ({ ...f, address: a.address, view_key: k.key }));
    } catch (e) { setMsg('Wallet error: ' + e.message); }
  };

  const act = (path, done) => async () => {
    setBusy(true); setMsg('');
    try {
      const j = await post(path, path === '/register' ? form : { address: form.address, view_key: form.view_key });
      setMsg(done(j)); load();
    } catch (e) { setMsg('Error: ' + e.message); }
    setBusy(false);
  };

  const totalBlocks = (rows || []).reduce((s, r) => s + r.blocks, 0);

  return (
    <div>
      <div className="card">
        <h2>Leaderboard</h2>
        <ServerPicker onChange={load} />
        <div className="stats">
          <div className="stat"><div className="muted">Participants</div><div>{rows ? rows.length : '-'}</div></div>
          <div className="stat"><div className="muted">Blocks counted</div><div>{rows ? totalBlocks : '-'}</div></div>
        </div>
        {err && <p className="muted">{err}</p>}
        {rows && rows.length === 0 && <p className="muted">Nobody has joined yet.</p>}
        {rows && rows.length > 0 && (
          <table>
            <thead><tr><th>#</th><th>Name</th><th>Address</th><th>Blocks</th><th>Rewards (SM64)</th><th>Last block</th><th>Best frames</th></tr></thead>
            <tbody>
              {rows.map((r, i) => (
                <tr key={r.address_short + i}>
                  <td>{i + 1}</td><td>{r.name}</td><td className="mono">{r.address_short}</td>
                  <td>{r.blocks}</td><td>{fmt(r.reward_atomic)}</td>
                  <td>{r.last_block_height ?? '-'}</td><td>{r.best_frames ?? '-'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      <div className="card">
        <h2>Join the leaderboard</h2>
        <p className="muted">
          Privacy note: sharing your private view key lets the leaderboard operator see all of your incoming
          payments. It cannot be used to spend your funds. Joining is optional, and anyone can run their own
          leaderboard (see the README).
        </p>
        <div className="kv"><span>Name</span><input value={form.name} onChange={set('name')} maxLength={32} placeholder="display name" /></div>
        <div className="kv"><span>Address</span><input className="mono" style={{ width: '100%' }} value={form.address} onChange={set('address')} placeholder="SM64..." /></div>
        <div className="kv"><span>View key</span><input className="mono" style={{ width: '100%' }} value={form.view_key} onChange={set('view_key')} placeholder="private view key (hex)" /></div>
        <p>
          <button onClick={useWallet} disabled={busy}>Use my open wallet</button>{' '}
          <button onClick={act('/register', (j) => `Joined: ${j.blocks} block(s) found so far.`)} disabled={busy}>Join</button>{' '}
          <button onClick={act('/unregister', () => 'Removed from the leaderboard.')} disabled={busy}>Leave (address + view key)</button>
        </p>
        {msg && <p className="muted">{msg}</p>}
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
