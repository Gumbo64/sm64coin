import React, { useEffect, useState } from 'react';
import { miner, isAddress } from './webminer/controller.js';
import { useMiner } from './webminer/useMiner.js';
import { mineGate, sessionOther, setSessionOther } from './walletGate.js';
import { hasWallet, createQuick } from './wallet/quick.js';

// The Mine page's front door: with no wallet on this device, a full-page "You don't have a wallet yet" instead of the miner
// (a wallet makes in one click; or mine for another address, remembered for the session). A registered wallet, opened or
// locked, goes straight to mining (its address is stored in the clear).
export default function MineGate({ children }) {
  const s = useMiner();
  const [known, setKnown] = useState(undefined);
  const [other, setOther] = useState(sessionOther());
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState('');
  useEffect(() => {
    let dead = false;
    if (isAddress(s.walletAddress || '')) setKnown(true);
    else hasWallet().then((k) => { if (!dead) setKnown(k); });
    return () => { dead = true; };
  }, [s.walletAddress]);
  const gate = mineGate({ walletKnown: known, other });
  if (gate === 'loading') return null;
  if (gate === 'mine') return children;
  const create = async () => {
    setBusy(true); setErr('');
    try { await createQuick(); setKnown(true); } catch (e) { setErr(e.message || String(e)); } finally { setBusy(false); }
  };
  const a = (draft || '').trim();
  const mineOther = () => { setSessionOther(a); setOther(a); miner.setAddress(a); };
  return (
    <div className="card gate" data-testid="no-wallet" style={{ textAlign: 'center', padding: '48px 18px' }}>
      <h2>You don't have a wallet yet</h2>
      <p className="muted">Mined coins need somewhere to go.</p>
      <p><button className="ui-big" style={{ maxWidth: 320 }} disabled={busy} onClick={create}>{busy ? 'Creating…' : 'Create wallet'}</button></p>
      {err && <p className="walleterr">{err}</p>}
      <p className="muted small" style={{ marginTop: 40 }}>
        Or mine for another address:{' '}
        <input className="mono" style={{ width: 'min(100%, 340px)' }} value={draft} placeholder="SM64…" onChange={(e) => setDraft(e.target.value)} />{' '}
        <button disabled={!isAddress(a)} onClick={mineOther}>Mine</button>
      </p>
    </div>
  );
}
