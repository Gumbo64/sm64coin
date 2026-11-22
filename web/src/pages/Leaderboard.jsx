import React, { useEffect, useState, useCallback, useMemo } from 'react';
import { Link } from 'react-router';
import { syncBoard, aggregate } from '../lib/chainBoard.js';

// Any leaderboard server can be used (anyone can run one: scripts/leaderboard.sh);
// '' means this site's own proxy.
const apiBase = () => localStorage.getItem('sm64coin.leaderboard') || '/leaderboard-api';
const fmt = (a) => (a / 1e12).toLocaleString(undefined, { maximumFractionDigits: 6 });

const SRC = 'sm64coin.lbsource';
const getSource = () => { try { return localStorage.getItem(SRC) === 'server' ? 'server' : 'chain'; } catch { return 'chain'; } };

export default function Leaderboard() {
  const [source, setSource] = useState(getSource);
  const pick = (s) => { try { localStorage.setItem(SRC, s); } catch { /* */ } setSource(s); };
  return (
    <div>
      <div className="card">
        <h2>Leaderboard</h2>
        <p className="tabs">
          <button className={source === 'chain' ? 'on' : ''} onClick={() => pick('chain')}>From the chain</button>{' '}
          <button className={source === 'server' ? 'on' : ''} onClick={() => pick('server')}>From a leaderboard server</button>
        </p>
        {source === 'chain' ? <ChainBoard /> : <ServerBoard />}
      </div>
      <div className="card">
        <h2>Join the leaderboard</h2>
        <p>
          <b>On the chain (no server):</b> opt in by putting your name and your address's private view key into the coinbase
          of the blocks you mine: on the <a href="/mine">Mine</a> page (&ldquo;Show me on the leaderboard&rdquo;), with the
          AutoMiner (<span className="mono">--leaderboard-name</span> / <span className="mono">--leaderboard-view-key-file</span>), or from the
          wallet app (Advanced &rarr; Leaderboard &rarr; On the chain). Every browser then computes this table from the
          blocks alone, and ignores a tag unless the block's coinbase really pays the tagged address.
        </p>
        <p className="muted">
          Privacy: a view key shows every payment to that address, to everyone, forever (it is in the block). It cannot
          spend your coins, but use a dedicated mining address (and not the one you keep your savings in).
          Publishing it on the chain cannot be undone; a server registration can at least be withdrawn.
        </p>
        <p><b>Or with a server:</b> in the wallet app, <b>Advanced &rarr; Leaderboard</b> can register the address and view key with a
          leaderboard server instead (the operator sees your incoming payments). This website never asks for your keys.</p>
      </div>
    </div>
  );
}

function ChainBoard() {
  const [state, setState] = useState({ entries: null, done: 0, total: 0, err: '' });
  useEffect(() => {
    let stop = false;
    const run = async () => {
      try {
        const r = await syncBoard((done, total, entries) => !stop && setState({ entries: [...entries], done, total, err: '' }), () => stop);
        if (!stop) setState({ entries: r.entries, done: r.scanned, total: r.tip, err: '' });
      } catch (e) { if (!stop) setState((s) => ({ ...s, err: 'Could not read the chain: ' + e.message })); }
    };
    run();
    const t = setInterval(run, 20000);
    return () => { stop = true; clearInterval(t); };
  }, []);
  const rows = useMemo(() => (state.entries ? aggregate(state.entries) : null), [state.entries]);
  const [open, setOpen] = useState('');
  const scanning = state.total && state.done < state.total;
  return (
    <>
      <p className="muted">Computed in your browser from the blocks alone: every coinbase with an opt-in tag whose view key
        proves that the block pays the tagged address. No server is trusted.{' '}
        {scanning ? `Scanning blocks ${state.done} / ${state.total}…` : state.total ? `Scanned ${state.total} blocks.` : ''}</p>
      <div className="stats">
        <div className="stat"><div className="muted">Participants</div><div>{rows ? rows.length : '-'}</div></div>
        <div className="stat"><div className="muted">Blocks counted</div><div>{rows ? rows.reduce((s, r) => s + r.blocks.length, 0) : '-'}</div></div>
      </div>
      {state.err && <p className="muted">{state.err}</p>}
      {rows && rows.length === 0 && !scanning && <p className="muted">No block carries a valid tag yet.</p>}
      {rows && rows.length > 0 && (
        <table>
          <thead><tr><th>#</th><th>Name</th><th>Address</th><th>Blocks</th><th>Rewards (SM64)</th><th>Last block</th><th>Best frames</th></tr></thead>
          <tbody>
            {rows.map((r, i) => (
              <React.Fragment key={r.address}>
                <tr>
                  <td>{i + 1}</td>
                  <td><button className="linkish" title="Show this miner's blocks" onClick={() => setOpen(open === r.address ? '' : r.address)}>{r.name}</button></td>
                  <td className="mono" title={r.address}>{r.address.slice(0, 8)}…{r.address.slice(-6)}<CopyButton text={r.address} /></td>
                  <td>{r.blocks.length}</td><td>{fmtBig(r.reward)}</td>
                  <td><Link to={`/block/${r.last}`}>{r.last}</Link></td><td>{r.bestFrames ?? '-'}</td>
                </tr>
                {open === r.address && (
                  <tr><td /><td colSpan={6} className="muted">Blocks: {r.blocks.map((h) => <span key={h}><Link to={`/block/${h}`}>{h}</Link>{' '}</span>)}</td></tr>
                )}
              </React.Fragment>
            ))}
          </tbody>
        </table>
      )}
    </>
  );
}

const fmtBig = (a) => (Number(a / 1000000n) / 1e6).toLocaleString(undefined, { maximumFractionDigits: 6 });

function ServerBoard() {
  const [rows, setRows] = useState(null);
  const [err, setErr] = useState('');

  const load = useCallback(() => {
    fetch(apiBase() + '/leaderboard').then((r) => r.json()).then((j) => { setRows(j); setErr(''); })
      .catch((e) => setErr('Leaderboard service unreachable: ' + e.message));
  }, []);
  useEffect(() => { load(); const t = setInterval(load, 15000); return () => clearInterval(t); }, [load]);

  const totalBlocks = (rows || []).reduce((s, r) => s + r.blocks, 0);

  return (
    <>
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
                  <td>{i + 1}</td><td>{r.name}{r.onchain ? <span className="muted" title="Also opted in on the chain"> ⛓</span> : null}</td>
                  <td className="mono" title={r.address || r.address_short}>{r.address_short}{r.address && <CopyButton text={r.address} />}</td>
                  <td>{r.blocks}</td><td>{fmt(r.reward_atomic)}</td>
                  <td>{r.last_block_height ?? '-'}</td><td>{r.best_frames ?? '-'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
    </>
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
