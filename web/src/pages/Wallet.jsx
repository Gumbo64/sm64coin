import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Link } from 'react-router';
import { getNode } from '../lib/rpc.js';
import { loadStored, saveStored, forgetStored } from '../lib/wallet/store.js';
import * as eng from '../lib/wallet/engine.js';

// A lite wallet in the browser: the fork's wallet2 (WebAssembly, in a web worker) scans the chain
// through the node's RPC. Keys are made and used inside the worker; what is stored (IndexedDB) is the
// wallet's own password-encrypted keys file and cache. Nothing but node RPC calls leaves the browser.
const SYNC_MS = 15000;
const short = (a) => (a ? `${a.slice(0, 10)}…${a.slice(-8)}` : '');
const isAddress = (a) => /^SM6[a-zA-Z0-9]{90,110}$/.test((a || '').trim());

export default function Wallet() {
  const [stage, setStage] = useState('loading'); // loading | none | locked | open
  const [rec, setRec] = useState(null);
  const [err, setErr] = useState('');
  const [busy, setBusy] = useState('');
  const [w, setW] = useState(null);
  const [viewOnly, setViewOnly] = useState(false);
  const [address, setAddress] = useState('');
  const [snap, setSnap] = useState(null);
  const [progress, setProgress] = useState(null);
  const [seed, setSeed] = useState('');
  const wRef = useRef(null);
  const syncing = useRef(false);
  const [engineErr, setEngineErr] = useState('');

  useEffect(() => { eng.loadLib().catch((e) => setEngineErr(e.message || String(e))); }, []);

  useEffect(() => {
    loadStored().then((r) => { setRec(r || null); setStage(r ? 'locked' : 'none'); }).catch(() => setStage('none'));
  }, []);

  const persist = useCallback(async (wallet, addr, vo) => {
    const data = await eng.exportData(wallet);
    await saveStored({ ...data, address: addr, viewOnly: vo });
  }, []);

  const refresh = useCallback(async (wallet) => {
    setSnap(await eng.snapshot(wallet));
  }, []);

  const sync = useCallback(async () => {
    const wallet = wRef.current;
    if (!wallet || syncing.current) return;
    syncing.current = true;
    try {
      await eng.syncOnce(wallet, (pct) => setProgress(pct));
      await refresh(wallet);
      await persist(wallet, await wallet.getPrimaryAddress(), await wallet.isViewOnly());
      setErr('');
    } catch (e) {
      setErr(`sync: ${e.message || e}`);
    } finally {
      syncing.current = false;
      setProgress(null);
    }
  }, [refresh, persist]);

  const adopt = useCallback(async (wallet, fresh) => {
    wRef.current = wallet;
    setW(wallet);
    const addr = await wallet.getPrimaryAddress();
    const vo = await wallet.isViewOnly();
    setAddress(addr);
    setViewOnly(vo);
    // the Mine and Auto-mine pages read this to know where the reward goes
    try { localStorage.setItem('sm64coin.address', addr); } catch { /* storage unavailable */ }
    if (fresh) await persist(wallet, addr, vo);
    setRec(await loadStored());
    setStage('open');
    sync();
  }, [persist, sync]);

  useEffect(() => {
    if (stage !== 'open') return undefined;
    const t = setInterval(sync, SYNC_MS);
    return () => clearInterval(t);
  }, [stage, sync]);

  useEffect(() => () => { const x = wRef.current; wRef.current = null; if (x) x.close(false).catch(() => {}); }, []);

  const run = async (label, fn) => {
    setErr(''); setBusy(label);
    try { await fn(); } catch (e) { setErr(e.message || String(e)); } finally { setBusy(''); }
  };

  const lock = async () => {
    const x = wRef.current;
    wRef.current = null; setW(null); setSnap(null); setSeed('');
    if (x) { try { await persist(x, address, viewOnly); await x.close(false); } catch { /* closing */ } }
    setStage('locked');
  };

  const forget = async () => {
    if (!window.confirm('Remove this wallet from this browser? Without its seed the coins are gone.')) return;
    const x = wRef.current;
    wRef.current = null; setW(null); setSnap(null); setSeed('');
    if (x) { try { await x.close(false); } catch { /* closing */ } }
    await forgetStored();
    setRec(null); setStage('none');
  };

  if (stage === 'loading') return <div className="card"><p className="muted">Loading…</p></div>;

  return (
    <div className="wallet">
      <div className="card">
        <h2>Wallet</h2>
        <p className="muted">
          A light wallet that runs in this browser: your keys are created and used here, encrypted with your
          password, and never sent anywhere. It scans the chain through the node ({getNode() || 'this site\'s proxy'}, change it on the{' '}
          <Link to="/network">Network</Link> page).
        </p>
        {engineErr && <p className="walleterr">{engineErr}</p>}
        {err && <p className="walleterr">{err}</p>}
        {busy && <p className="muted">{busy}…</p>}
      </div>
      {stage === 'none' && <Onboard run={run} adopt={adopt} busy={!!busy} />}
      {stage === 'locked' && <Unlock rec={rec} run={run} adopt={adopt} forget={forget} busy={!!busy} />}
      {stage === 'open' && (
        <Open w={w} address={address} viewOnly={viewOnly} snap={snap} progress={progress} seed={seed} setSeed={setSeed}
              sync={sync} lock={lock} forget={forget} run={run} refresh={() => refresh(wRef.current)} />
      )}
    </div>
  );
}

function Onboard({ run, adopt, busy }) {
  const [mode, setMode] = useState('create');
  const [pw, setPw] = useState('');
  const [pw2, setPw2] = useState('');
  const [seed, setSeed] = useState('');
  const [height, setHeight] = useState('0');
  const [addr, setAddr] = useState('');
  const [vk, setVk] = useState('');
  const [shown, setShown] = useState('');
  const [pending, setPending] = useState(null);
  const okPw = pw.length >= 8 && pw === pw2;

  const create = () => run('Creating the wallet', async () => {
    const wallet = await eng.createNew(pw);
    setShown(await wallet.getSeed());
    setPending(wallet);
  });
  const restore = () => run('Restoring the wallet', async () => {
    const h = Math.max(0, parseInt(height, 10) || 0);
    const wallet = mode === 'seed' ? await eng.restoreFromSeed(seed, pw, h) : await eng.restoreViewOnly(addr, vk, pw, h);
    await adopt(wallet, true);
  });

  if (pending) {
    return (
      <div className="card">
        <h3>Write down your seed</h3>
        <p>These 25 words are the only way to recover this wallet. Anyone who has them can spend your coins; if you lose them (and clear this browser's data) the coins are gone.</p>
        <p className="mono seedbox">{shown}</p>
        <button disabled={busy} onClick={() => run('Opening', async () => { await adopt(pending, true); setPending(null); setShown(''); })}>I have saved my seed, open the wallet</button>
      </div>
    );
  }
  return (
    <div className="card">
      <div className="controls">
        <button className={mode === 'create' ? 'on' : ''} onClick={() => setMode('create')}>Create a wallet</button>
        <button className={mode === 'seed' ? 'on' : ''} onClick={() => setMode('seed')}>Restore from seed</button>
        <button className={mode === 'view' ? 'on' : ''} onClick={() => setMode('view')}>View-only</button>
      </div>
      {mode === 'seed' && (
        <p><textarea className="wide mono" rows={3} placeholder="25 word seed" value={seed} onChange={(e) => setSeed(e.target.value)} /></p>
      )}
      {mode === 'view' && (
        <>
          <p className="muted">A view-only wallet sees incoming payments (mined rewards too) but cannot spend. It cannot see outgoing payments either, so its balance also counts coins the full wallet has already spent.</p>
          <p><input className="wide mono" placeholder="SM64… primary address" value={addr} onChange={(e) => setAddr(e.target.value)} /></p>
          <p><input className="wide mono" placeholder="private view key (64 hex)" value={vk} onChange={(e) => setVk(e.target.value)} /></p>
        </>
      )}
      {mode !== 'create' && (
        <p><label className="muted">Start scanning at block <input value={height} size={8} onChange={(e) => setHeight(e.target.value)} /> (0 is always right, a later block is faster)</label></p>
      )}
      <p>
        <input type="password" placeholder="password (8+ characters)" value={pw} onChange={(e) => setPw(e.target.value)} />{' '}
        <input type="password" placeholder="repeat it" value={pw2} onChange={(e) => setPw2(e.target.value)} />
      </p>
      {mode === 'create'
        ? <button disabled={!okPw || busy} onClick={create}>Create</button>
        : <button disabled={!okPw || busy || (mode === 'seed' ? seed.trim().split(/\s+/).length !== 25 : !isAddress(addr) || !/^[0-9a-fA-F]{64}$/.test(vk.trim()))} onClick={restore}>Restore</button>}
      <p className="muted">The password encrypts the wallet in this browser only; it can't be recovered, but the seed always can.</p>
    </div>
  );
}

function Unlock({ rec, run, adopt, forget, busy }) {
  const [pw, setPw] = useState('');
  const go = () => run('Unlocking', async () => {
    try { await adopt(await eng.openStored(rec, pw), false); }
    catch (e) { throw new Error(/password/i.test(e.message || '') ? 'Wrong password.' : e.message); }
  });
  return (
    <div className="card">
      <h3>Unlock</h3>
      <p className="mono break">{rec ? short(rec.address) : ''}{rec?.viewOnly ? ' (view-only)' : ''}</p>
      <input type="password" autoFocus placeholder="password" value={pw} onChange={(e) => setPw(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && pw && go()} />{' '}
      <button disabled={!pw || busy} onClick={go}>Unlock</button>{' '}
      <button className="linkish" onClick={forget}>forget this wallet</button>
    </div>
  );
}

function Open({ w, address, viewOnly, snap, progress, seed, setSeed, sync, lock, forget, run, refresh }) {
  return (
    <>
      <div className="card">
        <div className="label">Primary address</div>
        <p className="mono break addr">{address}</p>
        <div className="controls">
          <button onClick={() => navigator.clipboard && navigator.clipboard.writeText(address)}>Copy</button>
          <Link to={`/automine?address=${address}`}><button>Auto-mine to it</button></Link>
          <Link to={`/mine?address=${address}`}><button>Mine to it</button></Link>
        </div>
        <p className="muted">The Mine and Auto-mine pages now pay their rewards to this address.</p>
        <p className="muted">Balances and history come from the node you are connected to and are trusted as it reports them (a remote node could show wrong ones; it cannot spend your coins).</p>
      </div>
      <div className="stats">
        <div className="stat"><div className="label">Balance</div><div className="value">{snap ? eng.fmt(snap.balance) : '…'}</div></div>
        <div className="stat"><div className="label">Unlocked</div><div className="value">{snap ? eng.fmt(snap.unlocked) : '…'}</div></div>
        <div className="stat"><div className="label">Scanned to</div><div className="value">{snap ? `${snap.height} / ${snap.daemon}` : '…'}</div></div>
        <div className="stat"><div className="label">Sync</div><div className="value">{progress != null ? `${Math.round(progress * 100)}%` : 'idle'}</div></div>
      </div>
      <div className="card">
        <div className="controls">
          <button onClick={sync}>Sync now</button>
          <button onClick={async () => setSeed(seed ? '' : (viewOnly ? 'view-only wallet: no seed' : await w.getSeed()))}>{seed ? 'Hide seed' : 'Show seed'}</button>
          <button onClick={lock}>Lock</button>
          <button onClick={forget}>Forget</button>
        </div>
        {seed && <p className="mono seedbox">{seed}</p>}
      </div>
      {viewOnly && <p className="muted">View-only: spends made by the full wallet are not visible here, so this balance can be higher than the real one (change from a payment shows up as received).</p>}
      {!viewOnly && <Board w={w} run={run} />}
      <Txs snap={snap} />
      {!viewOnly && <Send w={w} snap={snap} run={run} refresh={refresh} />}
    </>
  );
}

function Txs({ snap }) {
  const rows = snap ? snap.rows : [];
  return (
    <div className="card">
      <h3>Transactions</h3>
      {rows.length === 0 ? <p className="muted">Nothing yet. Rewards appear once their block is scanned.</p> : (
        <table>
          <thead><tr><th>Block</th><th>Kind</th><th>Amount</th><th>Status</th><th>Tx</th></tr></thead>
          <tbody>
            {rows.map((r) => {
              const net = (r.incoming ?? 0n) - (r.outgoing ?? 0n);
              return (
                <tr key={r.hash}>
                  <td>{r.height ?? 'pool'}</td>
                  <td>{r.miner ? '★ mined reward' : net >= 0n ? 'received' : 'sent'}</td>
                  <td className="mono">{net >= 0n ? '+' : '-'}{eng.fmt(net >= 0n ? net : -net)}{net < 0n && r.fee ? ` (+${eng.fmt(r.fee)} fee)` : ''}</td>
                  <td>{r.failed ? 'failed' : !r.confirmed ? 'pending' : r.locked ? (r.unlockIn > 0 ? `unlocks in ${r.unlockIn} block${r.unlockIn === 1 ? '' : 's'}` : 'unlocking') : 'unlocked'}</td>
                  <td className="mono">{r.hash && <Link to={`/block/${r.height}`} title={r.hash}>{r.hash.slice(0, 10)}…</Link>}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </div>
  );
}

function Send({ w, snap, run, refresh }) {
  const [to, setTo] = useState('');
  const [amt, setAmt] = useState('');
  const [draft, setDraft] = useState(null);
  const [sent, setSent] = useState('');
  const atomic = eng.parseAmount(amt);
  const ok = isAddress(to) && atomic && atomic > 0n;

  const prepare = () => run('Building the transaction', async () => {
    setSent('');
    const tx = await w.createTx({ accountIndex: 0, address: to.trim(), amount: atomic, relay: false });
    setDraft({ tx, fee: tx.getFee(), amount: atomic, to: to.trim() });
  });
  const relay = () => run('Sending', async () => {
    const hash = await w.relayTx(draft.tx);
    setSent(hash); setDraft(null); setTo(''); setAmt('');
    await refresh();
  });

  return (
    <div className="card">
      <h3>Send</h3>
      <p><input className="wide mono" placeholder="SM64… recipient address" value={to} onChange={(e) => { setTo(e.target.value); setDraft(null); }} /></p>
      <p>
        <input placeholder="amount" value={amt} size={16} onChange={(e) => { setAmt(e.target.value); setDraft(null); }} />{' '}
        <span className="muted">spendable {snap ? eng.fmt(snap.unlocked) : '…'}</span>
      </p>
      {!draft && <button disabled={!ok} onClick={prepare}>Review</button>}
      {draft && (
        <div className="notice">
          Send <b>{eng.fmt(draft.amount)}</b> to <span className="mono break">{draft.to}</span>, network fee <b>{eng.fmt(draft.fee)}</b>.<br />
          <button onClick={relay}>Confirm and send</button>{' '}
          <button onClick={() => setDraft(null)}>Cancel</button>
        </div>
      )}
      {sent && <p>Sent: <span className="mono break">{sent}</span></p>}
    </div>
  );
}

const NAME_KEY = 'sm64coin.boardName';
function Board({ w, run }) {
  const [name, setName] = useState(() => { try { return localStorage.getItem(NAME_KEY) || ''; } catch { return ''; } });
  const [msg, setMsg] = useState('');
  const join = () => run('Signing and submitting', async () => {
    const n = name.trim();
    try { localStorage.setItem(NAME_KEY, n); } catch { /* storage unavailable */ }
    await eng.boardJoin(w, n);
    setMsg(`On the board as "${n}". Your node shares it with its peers.`);
  });
  return (
    <div className="card">
      <h3>Leaderboard</h3>
      <p className="walleterr">Joining shares this wallet's secret view key with the node, which can then see all incoming payments to it (not spend). Fine for your own node.</p>
      <p className="muted">
        Opt in to show your blocks on the <Link to="/leaderboard">Leaderboard</Link>. The entry is signed here with your
        spend key (only you can name this address); to count your blocks, nodes are given your address and secret view key,
        so anyone can then see all incoming payments to this wallet (not spend). Use a dedicated mining wallet. An entry
        cannot be removed once shared, only renamed.
      </p>
      <input placeholder="display name" value={name} maxLength={32} onChange={(e) => setName(e.target.value)} />{' '}
      <button disabled={!name.trim()} onClick={join}>Join / rename</button>
      {msg && <p>{msg}</p>}
    </div>
  );
}
