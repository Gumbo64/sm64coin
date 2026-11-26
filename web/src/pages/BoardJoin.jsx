import React, { useEffect, useState } from 'react';
import { Link } from 'react-router';
import { isChosenNode } from '../lib/rpc.js';
import { loadStored } from '../lib/wallet/store.js';
import * as eng from '../lib/wallet/engine.js';
import * as pk from '../lib/wallet/passkey.js';

// The leaderboard opt-in (Leaderboard page): the browser wallet saved on the Wallet page is opened here just long
// enough to sign the entry with its spend key and to list the blocks it mined (the node checks those heights with the
// view key it is given), then closed again. Same flow and warnings as the desktop wallet (gui/pages/Leaderboard.qml).
const NAME_KEY = 'sm64coin.boardName';

export default function BoardJoin({ onJoined = () => {} }) {
  const [rec, setRec] = useState(undefined); // undefined: loading, null: no wallet
  const [name, setName] = useState(() => { try { return localStorage.getItem(NAME_KEY) || ''; } catch { return ''; } });
  const [pw, setPw] = useState('');
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState('');
  const [msg, setMsg] = useState('');
  const [err, setErr] = useState('');
  useEffect(() => { loadStored().then((r) => setRec(r || null)).catch(() => setRec(null)); }, []);
  const node = eng.daemonUri();

  const join = async () => {
    const n = name.trim();
    setConfirm(false); setErr(''); setMsg('');
    try { localStorage.setItem(NAME_KEY, n); } catch { /* storage unavailable */ }
    let w = null;
    try {
      setBusy('Opening the wallet');
      const password = rec.passkey ? (await pk.openPasskey()).password : pw;
      try { w = await eng.openStored(rec, password); } catch { throw new Error(rec.passkey ? 'That passkey does not open this wallet.' : 'Wrong password.'); }
      setPw('');
      setBusy('Catching up with the chain (to list the blocks this wallet mined)');
      await eng.setNode(w);
      await eng.syncOnce(w, (pct) => setBusy(`Catching up with the chain (to list the blocks this wallet mined): ${Math.round(pct * 100)}%`));
      setBusy('Signing and submitting');
      await eng.boardJoin(w, n);
      setMsg(`On the board as "${n}". Your node shares it with its peers.`);
      onJoined();
    } catch (e) {
      setErr(e.message || String(e));
    } finally {
      if (w) w.close(false).catch(() => {});
      setBusy('');
    }
  };

  if (rec === undefined) return null;
  return (
    <div className="card">
      <h3>Join the leaderboard</h3>
      {!rec && <p className="muted">Create or load a wallet on the <Link to="/wallet">Wallet</Link> page and mine a block to it first; then join here.</p>}
      {rec && rec.viewOnly && <p className="muted">This browser's wallet is view-only: joining needs the spend key to sign the entry.</p>}
      {rec && !rec.viewOnly && (
        <>
          <p className="muted">
            Show the blocks of this browser's wallet (<span className="mono">{rec.address ? `${rec.address.slice(0, 8)}…${rec.address.slice(-6)}` : 'saved wallet'}</span>) under
            a name. The entry is signed with your spend key (only you can name this address); to count your blocks, nodes are given
            your address and view key, so anyone can then see all incoming payments to this wallet (not spend). Use a
            dedicated mining wallet. An entry cannot be removed once shared, only renamed.
          </p>
          {isChosenNode() && <p className="walleterr">That node was set by hand (Network page), not the site's default. Only continue if you run it or trust its operator.</p>}
          <input placeholder="display name" value={name} maxLength={32} disabled={!!busy} onChange={(e) => { setName(e.target.value); setConfirm(false); }} />{' '}
          {!confirm && !busy && <button disabled={!name.trim()} onClick={() => setConfirm(true)}>Join / rename…</button>}
          {confirm && (
            <div className="notice" data-testid="board-confirm">
              Join as <b>{name.trim()}</b>: this sends the wallet's <b>view key</b> to the node <b className="mono" data-testid="board-node">{node}</b>, with the address and the
              signed name. With the view key the node, and every node it shares the entry with, can see all incoming payments
              to this wallet; it cannot spend them. This cannot be undone.<br />
              {!rec.passkey && <><input type="password" autoFocus placeholder="wallet password" value={pw} onChange={(e) => setPw(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && pw && join()} />{' '}</>}
              <button disabled={!rec.passkey && !pw} onClick={join}>{rec.passkey ? 'Use the passkey, send the view key and join' : 'Send the view key and join'}</button>{' '}
              <button onClick={() => { setConfirm(false); setPw(''); }}>Cancel</button>
            </div>
          )}
          {busy && <p className="muted">{busy}…</p>}
          {msg && <p>{msg}</p>}
          {err && <p className="walleterr">{err}</p>}
        </>
      )}
    </div>
  );
}
