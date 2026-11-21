import React, { useState } from 'react';
import { wallet } from '../lib/rpc.js';
import { coins, ATOMIC, short, when } from '../lib/format.js';
import useInterval from '../lib/useInterval.js';

// A thin page over sm64coin-wallet-rpc (bin/; the forked wallet RPC). Run it
// with --wallet-dir; see the README. Keys never leave the wallet RPC.
export default function Wallet() {
  const [state, setState] = useState(null);
  const [err, setErr] = useState(null);
  const [name, setName] = useState('');
  const [pw, setPw] = useState('');
  const [to, setTo] = useState('');
  const [amt, setAmt] = useState('');
  const [msg, setMsg] = useState(null);

  const refresh = async () => {
    try {
      const [a, b, h, t] = await Promise.all([
        wallet('get_address', { account_index: 0 }),
        wallet('get_balance', { account_index: 0 }),
        wallet('get_height'),
        wallet('get_transfers', { in: true, out: true, pending: true, pool: true }),
      ]);
      const txs = [...(t.in || []), ...(t.out || []), ...(t.pending || []), ...(t.pool || [])]
        .sort((x, y) => (y.timestamp || 0) - (x.timestamp || 0));
      setState({ address: a.address, balance: b.balance, unlocked: b.unlocked_balance, blocks: b.blocks_to_unlock, height: h.height, txs });
      setErr(null);
    } catch (e) { setState(null); setErr(String(e)); }
  };
  useInterval(refresh, 8000);

  const open = async (create) => {
    try {
      await wallet(create ? 'create_wallet' : 'open_wallet', create ? { filename: name, password: pw, language: 'English' } : { filename: name, password: pw });
      setMsg(create ? `Created wallet "${name}". Back up its seed with sm64coin-wallet-cli (bin/).` : `Opened "${name}".`);
      refresh();
    } catch (e) { setMsg(String(e)); }
  };

  const send = async () => {
    try {
      const r = await wallet('transfer', { destinations: [{ address: to, amount: Math.round(Number(amt) * ATOMIC) }], account_index: 0 });
      setMsg(`Sent: tx ${r.tx_hash}, fee ${coins(r.fee)} SM64`);
      setTo(''); setAmt('');
      refresh();
    } catch (e) { setMsg(String(e)); }
  };

  return (
    <>
      {msg && <div className="card">{msg}</div>}
      {!state && (
        <div className="card">
          <h2>Wallet</h2>
          {err && <p className="muted">No wallet open ({err}).</p>}
          <label>Wallet name <input value={name} onChange={(e) => setName(e.target.value)} /></label>
          <label>Password <input type="password" value={pw} onChange={(e) => setPw(e.target.value)} /></label>
          <button disabled={!name} onClick={() => open(false)}>Open</button>{' '}
          <button disabled={!name} onClick={() => open(true)}>Create new</button>
        </div>
      )}
      {state && (
        <>
          <div className="stats">
            <div className="stat"><div className="label">Balance</div><div className="value">{coins(state.balance)} SM64</div></div>
            <div className="stat"><div className="label">Unlocked</div><div className="value">{coins(state.unlocked)} SM64</div></div>
            <div className="stat"><div className="label">Synced to</div><div className="value">{state.height}</div></div>
            {state.blocks > 0 && <div className="stat"><div className="label">Unlocks in</div><div className="value">{state.blocks} blocks</div></div>}
          </div>
          <div className="card">
            <div className="label">Your address</div>
            <div className="mono break">{state.address}</div>
          </div>
          <div className="card">
            <h3>Send</h3>
            <label>To <input className="mono wide" value={to} onChange={(e) => setTo(e.target.value.trim())} placeholder="SM64…" /></label>
            <label>Amount <input value={amt} onChange={(e) => setAmt(e.target.value)} placeholder="0.0" /></label>
            <button disabled={!to || !(Number(amt) > 0)} onClick={send}>Send</button>
            <p className="muted">Mined coins unlock after 10 blocks; rings need 16 outputs on the chain, so the very first transfers have to wait for ~20 blocks.</p>
          </div>
          <table>
            <thead><tr><th>When</th><th>Type</th><th>Amount</th><th>Height</th><th>Tx</th></tr></thead>
            <tbody>
              {state.txs.map((t) => (
                <tr key={t.txid + t.type}>
                  <td>{t.timestamp ? when(t.timestamp) : ''}</td>
                  <td>{t.type}</td>
                  <td>{t.type === 'out' ? '−' : '+'}{coins(t.amount)}</td>
                  <td>{t.height || 'pool'}</td>
                  <td className="mono">{short(t.txid, 16)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}
    </>
  );
}
