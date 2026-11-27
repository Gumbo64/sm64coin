import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Link } from 'react-router';
import { daemon, nodeInUse, onNodeChange } from '../lib/rpc.js';
import { fmt, fmtShort, parseAmount } from '../lib/wallet/engine.js';
import Identicon from '../components/Identicon.jsx';
import { miner } from '../lib/webminer/controller.js';
import { newPassword, NO_PASSWORD_WARNING } from '../lib/wallet/pwRule.js';
import { isNoPw, setNoPw } from '../lib/wallet/quick.js';
import { getPlatform } from '../platform/index.js';

// The desktop app's wallet: sm64coin-wallet-rpc (the wallet the Qt GUI uses) behind the Rust backend, which keeps its login to itself
// and enforces the password rules: the password opens a wallet, it locks after some idle minutes, sending asks for it again
// (setting), and the seed and keys are shown only after it. The browser build keeps the WASM wallet (Wallet.jsx).
const POLL_MS = 8000;
const short = (a) => (a ? `${a.slice(0, 10)}…${a.slice(-8)}` : '');
const isAddress = (a) => /^SM6[a-zA-Z0-9]{90,110}$/.test((a || '').trim());
const copy = (t) => { try { navigator.clipboard && navigator.clipboard.writeText(t); } catch { /* clipboard unavailable */ } };

export default function WalletDesktop() {
  const w = getPlatform().wallet;
  const [st, setSt] = useState(null);
  const [err, setErr] = useState('');
  const [busy, setBusy] = useState('');
  const [address, setAddress] = useState('');
  const [fresh, setFresh] = useState(null); // {seed, address} of a wallet just made: shown once
  const [note, setNote] = useState('');

  const refresh = useCallback(() => w.state().then(setSt).catch((e) => setErr(e.message)), [w]);
  useEffect(() => { refresh(); const t = setInterval(refresh, POLL_MS); return () => clearInterval(t); }, [refresh]);
  useEffect(() => w.onLocked(() => { setAddress(''); setNote('The wallet locked itself after a while without use. Enter the password to open it again.'); refresh(); }), [w, refresh]);

  // User activity keeps the wallet open (the auto-lock timer lives in Rust); throttled to one call per 5 s.
  useEffect(() => {
    let last = 0;
    const ping = () => { const n = Date.now(); if (n - last > 5000) { last = n; w.activity(); } };
    const evs = ['mousemove', 'keydown', 'click', 'wheel'];
    evs.forEach((e) => window.addEventListener(e, ping, { passive: true }));
    return () => evs.forEach((e) => window.removeEventListener(e, ping));
  }, [w]);

  // The wallet reads the chain from the same node as the rest of the app (the local one while it answers).
  const point = useCallback(async () => {
    const n = nodeInUse();
    try {
      if (n.kind === 'local') { const u = getPlatform().nodeUrl(); const m = /:(\d+)$/.exec(u || ''); if (m) await w.setNode(`127.0.0.1:${m[1]}`); }
      else if (n.kind === 'public' && n.url) await w.setNode(n.url);
      else if (n.kind === 'chosen' && /^https:\/\//.test(n.url)) await w.setNode(n.url);
    } catch { /* not open yet: the sidecar starts with the right node */ }
  }, [w]);
  useEffect(() => { if (st && st.open) point(); return onNodeChange(point); }, [st && st.open, point]); // eslint-disable-line react-hooks/exhaustive-deps

  const run = async (label, f) => {
    setBusy(label); setErr('');
    try { return await f(); } catch (e) { setErr(e.message || String(e)); } finally { setBusy(''); refresh(); }
  };
  const adopt = (a, force = false) => { setAddress(a); setNote(''); miner.noteWallet(a, force); };

  if (!st) return <div className="card"><h2>Wallet</h2><p className="muted">{err || 'Starting…'}</p></div>;
  if (!st.available) return <div className="card"><h2>Wallet</h2><p className="walleterr">The wallet component (sm64coin-wallet-rpc) is not in this app's folder.</p></div>;

  return (
    <>
      {err && <p className="walleterr" role="alert">{err}</p>}
      {busy && <p className="muted">{busy}…</p>}
      {fresh && <NewSeed fresh={fresh} done={() => { adopt(fresh.address); setFresh(null); }} />}
      {!fresh && !st.open && st.wallets.length > 0 && <Unlock st={st} note={note} run={run} adopt={adopt} busy={!!busy} />}
      {!fresh && !st.open && <Onboard st={st} run={run} setFresh={setFresh} adopt={adopt} busy={!!busy} first={st.wallets.length === 0} />}
      {!fresh && st.open && <Open st={st} w={w} address={address} setAddress={setAddress} run={run} refresh={refresh} />}
    </>
  );
}

function NewSeed({ fresh, done }) {
  const [ok, setOk] = useState(false);
  return (
    <div className="card">
      <h2>Write down your seed words</h2>
      <p className="walleterr">These 25 words are the only backup. Anyone who has them can spend your coins; without them a lost password or disk means lost coins.</p>
      <p className="mono seedbox" data-testid="new-seed">{fresh.seed}</p>
      <p className="mono break">{fresh.address}</p>
      <label><input type="checkbox" checked={ok} onChange={(e) => setOk(e.target.checked)} /> I have written them down and keep them safe</label>{' '}
      <button disabled={!ok} onClick={done}>Continue</button>
    </div>
  );
}

function Unlock({ st, note, run, adopt, busy }) {
  const w = getPlatform().wallet;
  const [name, setName] = useState(st.wallets[0]);
  const [pw, setPw] = useState('');
  const go = () => run('Opening', async () => { const r = await w.open(name, pw); setPw(''); adopt(r.address); });
  // a wallet made without a password opens by itself
  useEffect(() => { if (isNoPw(name) && !busy) go(); }, [name]); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <div className="card">
      <h2>Wallet</h2>
      {note && <p className="notice">{note}</p>}
      <p>
        {st.wallets.length > 1 && <select value={name} onChange={(e) => setName(e.target.value)}>{st.wallets.map((n) => <option key={n}>{n}</option>)}</select>}
        {st.wallets.length === 1 && <b>{name}</b>}
      </p>
      {/* a form: Enter submits it */}
      <form onSubmit={(e) => { e.preventDefault(); if (!busy) go(); }}>
        <input type="password" autoFocus placeholder="wallet password" value={pw} onChange={(e) => setPw(e.target.value)} />{' '}
        <button type="submit" disabled={busy}>Open</button>
      </form>
      <p className="muted">Stored in {st.dir} (readable by you only).</p>
    </div>
  );
}

function Onboard({ st, run, setFresh, adopt, busy, first }) {
  const w = getPlatform().wallet;
  const [mode, setMode] = useState(first ? 'home' : null); // home (first wallet: one click) | create | restore | passkey
  const [name, setName] = useState(first ? 'main' : '');
  const [pw, setPw] = useState('');
  const [pw2, setPw2] = useState('');
  const [seed, setSeed] = useState('');
  const [height, setHeight] = useState('0');
  const [offset, setOffset] = useState('');
  const [kdf, setKdf] = useState('1');
  const [adv, setAdv] = useState(false);
  if (mode === 'home') {
    return (
      <div className="card" data-testid="onboard-home">
        <p><button className="ui-big" style={{ maxWidth: 320 }} disabled={busy} onClick={() => run('Creating the wallet', async () => { const r = await w.create({ name: 'main', password: '' }); setNoPw('main', true); adopt(r.address, true); })}>Create wallet</button></p>
        <p><button disabled={busy} onClick={() => setMode('passkey')}>Use my website passkey</button></p>
        <p className="small"><button className="linkish" onClick={() => setMode('restore')}>Restore a wallet…</button></p>
      </div>
    );
  }
  if (!mode) return <div className="card"><button onClick={() => setMode('create')}>Create another wallet…</button>{' '}<button onClick={() => setMode('restore')}>Restore from seed…</button>{' '}<button onClick={() => setMode('passkey')}>Use my website passkey…</button></div>;
  if (mode === 'passkey') return <Passkey st={st} run={run} adopt={adopt} busy={busy} first={first} back={() => setMode(first ? 'home' : null)} />;
  const nameOk = /^[A-Za-z0-9_-]{1,40}$/.test(name) && !st.wallets.includes(name);
  const np = newPassword(pw, pw2);
  const pwOk = np.ok;
  const ok = nameOk && pwOk && (mode === 'create' || seed.trim().split(/\s+/).length >= 24);
  const kdfN = Math.max(1, Math.min(1000000, parseInt(kdf, 10) || 1));
  const go = () => run(mode === 'create' ? 'Creating the wallet' : 'Restoring (this scans the chain from the restore height)', async () => {
    if (mode === 'create') { const r = await w.create({ name, password: pw, kdfRounds: kdfN }); setNoPw(name, !pw); setPw(''); setPw2(''); setFresh(r); } else {
      const r = await w.restore({ name, password: pw, seed, restoreHeight: height, seedOffset: offset, kdfRounds: kdfN }); setNoPw(name, !pw); setPw(''); setPw2(''); setSeed(''); adopt(r.address, true);
    }
  });
  return (
    <div className="card">
      <h2>{first ? 'Wallet' : mode === 'create' ? 'New wallet' : 'Restore a wallet'}</h2>
      {first && (
        <div className="controls">
          <button className="linkish" onClick={() => setMode('home')}>◂ back</button>
          <button className={mode === 'restore' ? '' : 'linkish'} onClick={() => setMode('restore')}>Restore from seed words</button>
          <button className="linkish" onClick={() => setMode('passkey')}>Use my website passkey</button>
        </div>
      )}
      <p><input placeholder="wallet name" value={name} onChange={(e) => setName(e.target.value)} />{!nameOk && name && <span className="muted"> letters, digits, - _ (not used yet)</span>}</p>
      {mode === 'restore' && (
        <>
          <p><textarea className="wide mono" style={{ background: "var(--bg-input, #0b0f19)", color: "inherit", border: "1px solid #2a2f45", borderRadius: 6, padding: 8 }} rows={3} placeholder="25 seed words" value={seed} onChange={(e) => setSeed(e.target.value)} /></p>
          <p><input placeholder="restore height" size={12} value={height} onChange={(e) => setHeight(e.target.value)} /> <span className="muted">block to scan from (0 = the start: always right, slower)</span></p>
        </>
      )}
      <p><input type="password" placeholder="password (8+ characters, optional)" value={pw} onChange={(e) => setPw(e.target.value)} />{' '}
        <input type="password" placeholder="repeat it" value={pw2} onChange={(e) => setPw2(e.target.value)} /></p>
      {np.error ? <p className="walleterr" data-testid="pw-error">{np.error}</p> : np.warn ? <p className="muted small" data-testid="pw-warn">{np.warn}</p> : null}
      <p><button className="linkish" onClick={() => setAdv(!adv)}>{adv ? '▾' : '▸'} advanced</button></p>
      {adv && (
        <p>
          <input placeholder="password hashing rounds" size={10} value={kdf} onChange={(e) => setKdf(e.target.value)} /> <span className="muted">KDF rounds (default 1; more makes a stolen wallet file harder to crack and opening slower: this app remembers the count next to the wallet file, as &lt;name&gt;.kdf, and opens it with it)lt;namethe same count is needed nowhere else, it is stored in the file)gt;.kdf)</span>
          {mode === 'restore' && <><br /><input type="password" placeholder="seed offset passphrase (optional)" value={offset} onChange={(e) => setOffset(e.target.value)} /></>}
        </p>
      )}
      <button disabled={busy || !ok} onClick={go}>{mode === 'create' ? 'Create' : 'Restore'}</button>
      {!first && <>{' '}<button className="linkish" onClick={() => setMode(null)}>cancel</button></>}
    </div>
  );
}

function Open({ st, w, address, setAddress, run, refresh }) {
  const [bal, setBal] = useState(null);
  const [txs, setTxs] = useState([]);
  const [heights, setHeights] = useState({ wallet: 0, chain: 0 });
  const [shown, setShown] = useState(false); // the balance is revealed
  const hide = st.settings.hide_balance && !shown;

  const poll = useCallback(async () => {
    try {
      const [b, t, h] = await Promise.all([w.balance(), w.transfers(), w.height()]);
      let chain = 0;
      try { chain = (await daemon('get_info')).height; } catch { /* node down */ }
      setBal(b); setTxs(t); setHeights({ wallet: h, chain });
    } catch (e) { if (e.code === 'wallet_not_open' || e.code === 'wallet_not_running') refresh(); }
  }, [w, refresh]);
  useEffect(() => { poll(); const t = setInterval(poll, 8000); return () => clearInterval(t); }, [poll]);

  const syncPct = heights.chain > 0 ? Math.min(100, Math.floor((heights.wallet * 100) / heights.chain)) : null;
  const val = (a) => (hide ? '•••••' : a == null ? '…' : fmtShort(a));
  const [, bump] = useState(0);
  const lock = () => run('Locking', async () => { await w.close(); setAddress(''); });

  return (
    <>
      <div className="card">
        <h2>Wallet <span className="muted">{st.open}</span></h2>
        <AddressRow w={w} address={address} setAddress={setAddress} />
        {address && !isBacked(address) && <p className="notice small" data-testid="backup-reminder">Back up your seed words: <b>Show seed words and keys</b> below.</p>}
        <div className="controls">
          <button onClick={lock}>Lock</button>{' '}
          {st.settings.hide_balance && <button className="linkish" onClick={() => setShown(!shown)}>{hide ? 'Show balance' : 'Hide balance'}</button>}
        </div>
      </div>
      <div className="stats">
        <div className="stat"><div className="label">Balance</div><div className="value" title={!hide && bal ? fmt(bal.balance) : undefined}>{val(bal && bal.balance)}</div></div>
        <div className="stat"><div className="label">Unlocked</div><div className="value" title={!hide && bal ? fmt(bal.unlocked) : undefined}>{val(bal && bal.unlocked)}</div></div>
        <div className="stat"><div className="label">Scanned to</div><div className="value">{heights.chain ? `${heights.wallet} / ${heights.chain}` : '…'}</div></div>
        <div className="stat"><div className="label">Sync</div><div className="value">{syncPct == null ? '…' : syncPct >= 100 ? 'synced' : `${syncPct}%`}</div></div>
      </div>
      <Txs txs={txs} hide={hide} />
      <Send w={w} bal={bal} settings={st.settings} run={run} poll={poll} hide={hide} />
      <Keys w={w} run={run} onShown={() => { markBacked(address); bump(); }} />
      <SetPassword w={w} name={st.open} run={run} />
      <Settings w={w} st={st} run={run} refresh={refresh} />
    </>
  );
}

function AddressRow({ w, address, setAddress }) {
  useEffect(() => { if (!address) w.address().then(setAddress).catch(() => {}); }, [address, w, setAddress]);
  return (
    <>
      <div className="label">Primary address</div>
      <p className="mono break addr">{address && <Identicon address={address} size={32} />} {address || '…'}</p>
      <div className="controls">
        <button disabled={!address} onClick={() => copy(address)}>Copy</button>{' '}
        {address && <Link to="/mine" onClick={() => miner.setAddress(address)}><button>Mine to it</button></Link>}
      </div>
      <p className="muted">The Mine page can pay its rewards to this address. Balances come from the node this app reads from ({nodeInUse().kind === 'public' ? 'the public node' : 'its own node'}).</p>
    </>
  );
}

function Txs({ txs, hide }) {
  return (
    <div className="card">
      <h3>Transactions</h3>
      {txs.length === 0 ? <p className="muted">Nothing yet. Rewards appear once their block is scanned.</p> : (
        <table>
          <thead><tr><th>Block</th><th>Kind</th><th>Amount</th><th>Status</th><th>Tx</th></tr></thead>
          <tbody>
            {txs.map((t) => {
              const out = t.type === 'out' || (t.type === 'pending');
              const kind = t.type === 'block' ? '★ mined reward' : out ? 'sent' : 'received';
              const status = t.type === 'failed' ? 'failed' : t.type === 'pool' || t.type === 'pending' ? 'pending' : `${t.confirmations ?? 0} confirmations`;
              return (
                <tr key={t.txid + t.type}>
                  <td>{t.height || 'pool'}</td>
                  <td>{kind}</td>
                  <td className="mono">{hide ? '•••••' : `${out ? '-' : '+'}${fmt(t.amount)}`}{!hide && out && t.fee && t.fee !== '0' ? ` (+${fmt(t.fee)} fee)` : ''}</td>
                  <td>{status}</td>
                  <td className="mono">{t.txid && t.height ? <Link to={`/block/${t.height}`} title={t.txid}>{t.txid.slice(0, 10)}…</Link> : (t.txid || '').slice(0, 10)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </div>
  );
}

function Send({ w, bal, settings, run, poll, hide }) {
  const [to, setTo] = useState('');
  const [amt, setAmt] = useState('');
  const [pw, setPw] = useState('');
  const [draft, setDraft] = useState(null);
  const [sent, setSent] = useState('');
  const atomic = parseAmount(amt);
  const ok = isAddress(to) && atomic && atomic > 0n && true; // the app checks the password (an empty one is right for a wallet made without one)
  const prepare = () => run('Building the transaction', async () => {
    setSent('');
    const d = await w.draft({ address: to.trim(), amount: atomic, password: pw });
    setPw('');
    setDraft({ ...d, to: to.trim() });
  });
  const relay = () => run('Sending', async () => {
    const hash = await w.relay(draft.metadata);
    setSent(hash); setDraft(null); setTo(''); setAmt('');
    poll();
  });
  return (
    <div className="card">
      <h3>Send</h3>
      <p><input className="wide mono" placeholder="SM64… recipient address" value={to} onChange={(e) => { setTo(e.target.value); setDraft(null); }} /></p>
      {isAddress(to) && <p className="muted small" data-testid="to-preview"><Identicon address={to.trim()} size={24} /> {to.trim().slice(0, 8)}…{to.trim().slice(-6)}: check it is the right one.</p>}

      <p>
        <input placeholder="amount" value={amt} size={16} onChange={(e) => { setAmt(e.target.value); setDraft(null); }} />{' '}
        <span className="muted">spendable {hide ? '•••••' : bal ? fmtShort(bal.unlocked) : '…'}</span>
      </p>
      {settings.password_on_send && !draft && <p><input type="password" placeholder="wallet password" value={pw} onChange={(e) => setPw(e.target.value)} /></p>}
      {!draft && <button disabled={!ok} onClick={prepare}>Review</button>}
      {draft && (
        <div className="notice">
          Send <b>{fmt(draft.amount)}</b> to <span className="mono break">{draft.to}</span>, network fee <b>{fmt(draft.fee)}</b>.<br />
          <button onClick={relay}>Confirm and send</button>{' '}
          <button onClick={() => setDraft(null)}>Cancel</button>
        </div>
      )}
      {sent && <p>Sent: <span className="mono break">{sent}</span></p>}
    </div>
  );
}

function Keys({ w, run, onShown }) {
  const [open, setOpen] = useState(false);
  const [pw, setPw] = useState('');
  const [k, setK] = useState(null);
  const close = () => { setOpen(false); setPw(''); setK(null); };
  const reveal = () => run('Checking the password', async () => {
    const seed = await w.seed(pw);
    const keys = await w.keys(pw);
    setK({ seed, ...keys }); setPw(''); onShown && onShown();
  });
  return (
    <div className="card">
      {!open && <button onClick={() => setOpen(true)}>Show seed words and keys…</button>}
      {open && !k && (
        <>
          <p className="walleterr">Anyone who sees the spend key or the seed words can spend all of this wallet's coins. Show them only where nobody can look over your shoulder, and never paste them into a website or send them to anyone.</p>
          <form onSubmit={(e) => { e.preventDefault(); reveal(); }}>
            <input type="password" autoFocus placeholder="wallet password" value={pw} onChange={(e) => setPw(e.target.value)} />{' '}
            <button type="submit">I understand, show them</button>{' '}
            <button type="button" className="linkish" onClick={close}>cancel</button>
          </form>
        </>
      )}
      {k && (
        <>
          <p className="walleterr">Secret: whoever has these can spend your coins.</p>
          <p><span className="label">Seed words (25)</span><br /><span className="mono break seedbox">{k.seed}</span></p>
          <p><span className="label">Private spend key</span><br /><span className="mono break seedbox">{k.spendKey}</span></p>
          <p><span className="label">Private view key</span><br /><span className="mono break seedbox">{k.viewKey}</span></p>
          <button onClick={close}>Hide</button>
        </>
      )}
    </div>
  );
}

const BK = (a) => `sm64coin.backedup.${a}`;
const isBacked = (a) => { try { return !!localStorage.getItem(BK(a)); } catch { return false; } };
const markBacked = (a) => { try { if (a) localStorage.setItem(BK(a), String(Date.now())); } catch { /* storage unavailable */ } };

/** Set (or change) the wallet's password, later: nothing asks for one while the visitor plays or explores. */
function SetPassword({ w, name, run }) {
  const none = isNoPw(name);
  const [open, setOpen] = useState(false);
  const [cur, setCur] = useState('');
  const [pw, setPw] = useState('');
  const [pw2, setPw2] = useState('');
  const np = newPassword(pw, pw2);
  const save = () => run('Setting the password', async () => { await w.setPassword(none ? '' : cur, pw); setNoPw(name, false); setOpen(false); setCur(''); setPw(''); setPw2(''); });
  return (
    <div className="card">
      <button className="linkish" onClick={() => setOpen(!open)}>{open ? '▾' : '▸'} {none ? 'Set a password' : 'Change password'}</button>
      {open && (
        <>
          {none && <p className="muted small">{NO_PASSWORD_WARNING}</p>}
          {!none && <p><input type="password" placeholder="current password" value={cur} onChange={(e) => setCur(e.target.value)} /></p>}
          <p><input type="password" placeholder="new password (8+ characters)" value={pw} onChange={(e) => setPw(e.target.value)} />{' '}
            <input type="password" placeholder="repeat it" value={pw2} onChange={(e) => setPw2(e.target.value)} /></p>
          {(pw || pw2) && np.error ? <p className="walleterr" data-testid="pw-error">{np.error}</p> : null}
          <button disabled={!pw || !np.ok} onClick={save}>Save password</button>
        </>
      )}
    </div>
  );
}

function Settings({ w, st, run, refresh }) {
  const [open, setOpen] = useState(false);
  const [s, setS] = useState(st.settings);
  const [pw, setPw] = useState('');
  useEffect(() => { setS(st.settings); }, [st.settings]);
  const save = () => run('Saving', async () => { await w.settings({ ...s, auto_lock_minutes: Math.max(0, parseInt(s.auto_lock_minutes, 10) || 0) }, pw); setPw(''); refresh(); });
  return (
    <div className="card">
      <button className="linkish" onClick={() => setOpen(!open)}>{open ? '▾' : '▸'} Wallet settings</button>
      {open && (
        <>
          <p><label>Lock after <input size={4} value={s.auto_lock_minutes} onChange={(e) => setS({ ...s, auto_lock_minutes: e.target.value })} /> idle minutes (0 = never)</label></p>
          <p><label><input type="checkbox" checked={s.password_on_send} onChange={(e) => setS({ ...s, password_on_send: e.target.checked })} /> Ask for the password before sending</label></p>
          <p><label><input type="checkbox" checked={s.hide_balance} onChange={(e) => setS({ ...s, hide_balance: e.target.checked })} /> Hide the balance until clicked</label></p>
          <p className="muted">Making a protection weaker (longer or no lock, no password to send) asks for the wallet password.</p>
          <p><input type="password" placeholder="wallet password (only to weaken)" value={pw} onChange={(e) => setPw(e.target.value)} />{' '}<button onClick={save}>Save</button></p>
        </>
      )}
    </div>
  );
}

/** Hand-off of the website's passkey wallet (see lib/desktopLink.js): the browser derives it, the app restores it under a password chosen here. */
function Passkey({ st, run, adopt, busy, back }) {
  const p = getPlatform();
  const [name, setName] = useState(st.wallets.includes('passkey') ? '' : 'passkey');
  const [pw, setPw] = useState('');
  const [pw2, setPw2] = useState('');
  const [link, setLink] = useState(null); // {state, url}
  const [got, setGot] = useState(''); // the address that arrived: the user compares it with the browser page before importing
  const nameOk = /^[A-Za-z0-9_-]{1,40}$/.test(name) && !st.wallets.includes(name);
  const np = newPassword(pw, pw2);
  const ok = nameOk && np.ok;
  const waiting = link && link.state === 'waiting';

  useEffect(() => {
    if (!waiting) return undefined;
    const t = setInterval(async () => {
      const s = await p.link.status().catch(() => null);
      if (!s) return;
      if (s.state === 'received') {
        clearInterval(t);
        setLink(null);
        setGot(s.address || '');
      } else if (s.state !== 'waiting') { clearInterval(t); setLink({ state: s.state, error: s.error }); }
    }, 1500);
    return () => clearInterval(t);
  }, [waiting]); // eslint-disable-line react-hooks/exhaustive-deps

  const start = () => run('Opening your browser', async () => setLink(await p.link.start(true)));
  const cancel = async () => { await p.link.cancel().catch(() => {}); setLink(null); setGot(''); };
  const finish = () => run('Restoring the passkey wallet (this scans the chain)', async () => {
    await p.link.finish({ name, password: pw, address: got }); setNoPw(name, !pw);
    setPw(''); setPw2(''); setGot(''); adopt('');
  });
  return (
    <div className="card">
      <h2>Use my website passkey</h2>
      <p className="muted">Your browser opens the SM64Coin site, where you use your passkey; the wallet is then handed to this app (encrypted, over 127.0.0.1) and kept here under the password you choose now. Needs the passkey you made on the website.</p>
      {got && (
        <>
          <p>The app received this wallet:</p>
          <p className="mono break"><b>{got}</b></p>
          <p>Is it the address your browser page shows? Import it only if it is the same. If it is not, or the page said sending failed, press Cancel: something else on this computer answered first.</p>
          <button disabled={busy} onClick={finish}>Same address: import it</button>{' '}
          <button className="linkish" onClick={cancel}>Cancel</button>
        </>
      )}
      {!waiting && !got && (
        <>
          <p><input placeholder="wallet name" value={name} onChange={(e) => setName(e.target.value)} /></p>
          <p><input type="password" placeholder="password for this app (8+ characters, optional)" value={pw} onChange={(e) => setPw(e.target.value)} />{' '}
            <input type="password" placeholder="repeat it" value={pw2} onChange={(e) => setPw2(e.target.value)} /></p>
          {np.error ? <p className="walleterr" data-testid="pw-error">{np.error}</p> : np.warn ? <p className="muted small">{np.warn}</p> : null}
          <button disabled={busy || !ok} onClick={start}>Open the browser</button>{' '}
          <button className="linkish" onClick={back}>back</button>
        </>
      )}
      {waiting && (
        <>
          {link.error && <p className="walleterr">{link.error}</p>}
          <p>Waiting for the browser… Use your passkey on the page that opened (this wait ends after 5 minutes).</p>
          <p className="muted">If no browser opened, copy this link into it: <span className="mono break">{link.url}</span> <button onClick={() => copy(link.url)}>Copy</button></p>
          <button onClick={cancel}>Cancel</button>
        </>
      )}
      {link && !waiting && link.state !== 'idle' && <p className="walleterr">{link.error || `The link ${link.state}.`}</p>}
    </div>
  );
}
