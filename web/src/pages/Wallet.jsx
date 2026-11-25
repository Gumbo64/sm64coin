import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Link } from 'react-router';
import { getNode } from '../lib/rpc.js';
import { loadStored, saveStored, forgetStored } from '../lib/wallet/store.js';
import * as eng from '../lib/wallet/engine.js';
import * as pk from '../lib/wallet/passkey.js';

// A lite wallet in the browser: the fork's wallet2 (WebAssembly, in a web worker) scans the chain
// through the node's RPC. Keys are made and used inside the worker; what is stored (IndexedDB) is the
// wallet's own password-encrypted keys file and cache. Nothing but node RPC calls leaves the browser.
const SYNC_MS = 15000;
const short = (a) => (a ? `${a.slice(0, 10)}…${a.slice(-8)}` : '');
const isHex64 = (k) => /^[0-9a-fA-F]{64}$/.test((k || '').trim());
const BK = (a) => `sm64coin.backedup.${a}`;
const isBackedUp = (a) => { try { return !!localStorage.getItem(BK(a)); } catch { return false; } };
const markBackedUp = (a) => { try { localStorage.setItem(BK(a), String(Date.now())); } catch { /* storage unavailable */ } };
const readFile = (f) => new Promise((resolve, reject) => {
  const r = new FileReader();
  r.onload = () => resolve(new Uint8Array(r.result));
  r.onerror = () => reject(r.error || new Error('could not read the file'));
  r.readAsArrayBuffer(f);
});
/** Save bytes/text as a download: a Blob and a temporary link, nothing is uploaded. */
const download = (name, data, type) => {
  const url = URL.createObjectURL(new Blob([data], { type }));
  const a = document.createElement('a');
  a.href = url; a.download = name;
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 5000);
};
const copy = (t) => navigator.clipboard && navigator.clipboard.writeText(t);
const wrongPw = (e) => new Error(/password/i.test(e.message || '') ? 'Wrong password (or this is not a wallet file).' : e.message);
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
  const passkeyRef = useRef(false);
  const [passkey, setPasskey] = useState(false);
  const syncing = useRef(false);
  const [engineErr, setEngineErr] = useState('');

  useEffect(() => { eng.loadLib().catch((e) => setEngineErr(e.message || String(e))); }, []);

  useEffect(() => {
    loadStored().then((r) => { setRec(r || null); setStage(r ? 'locked' : 'none'); }).catch(() => setStage('none'));
  }, []);

  const persist = useCallback(async (wallet, addr, vo) => {
    const data = await eng.exportData(wallet);
    await saveStored({ ...data, address: addr, viewOnly: vo, passkey: passkeyRef.current });
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

  const adopt = useCallback(async (wallet, fresh, viaPasskey) => {
    passkeyRef.current = !!viaPasskey;
    setPasskey(!!viaPasskey);
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
        <Open w={w} passkey={passkey} address={address} viewOnly={viewOnly} snap={snap} progress={progress} seed={seed} setSeed={setSeed}
              sync={sync} lock={lock} forget={forget} run={run} refresh={() => refresh(wRef.current)} />
      )}
    </div>
  );
}

function Onboard({ run, adopt, busy }) {
  const [pkOk, setPkOk] = useState(false);
  useEffect(() => { pk.supported().then(setPkOk); }, []);
  const pkCreate = () => run('Creating the passkey wallet', async () => {
    const k = await pk.createPasskey();
    await adopt(await eng.restoreFromSpendKey(k.spendKey, k.password, k.height), true, true);
  });
  const pkOpen = () => run('Opening with the passkey', async () => {
    const k = await pk.openPasskey();
    await adopt(await eng.restoreFromSpendKey(k.spendKey, k.password, k.height), true, true);
  });
  const [mode, setMode] = useState('create');
  const [pw, setPw] = useState('');
  const [pw2, setPw2] = useState('');
  const [seed, setSeed] = useState('');
  const [offset, setOffset] = useState('');
  const [height, setHeight] = useState('0');
  const [addr, setAddr] = useState('');
  const [vk, setVk] = useState('');
  const [sk, setSk] = useState('');
  const [keysFile, setKeysFile] = useState(null);
  const [cacheFile, setCacheFile] = useState(null);
  const fresh = mode === 'create' || mode === 'seed' || mode === 'keys' || mode === 'view';
  const okPw = fresh ? pw.length >= 8 && pw === pw2 : pw.length > 0;

  const go = () => run({ create: 'Creating the wallet', seed: 'Restoring the wallet', keys: 'Restoring the wallet', view: 'Restoring the wallet', backup: 'Opening the backup', desktop: 'Opening the desktop wallet' }[mode], async () => {
    const h = Math.max(0, parseInt(height, 10) || 0);
    let wallet;
    if (mode === 'create') wallet = await eng.createNew(pw);
    else if (mode === 'seed') wallet = await eng.restoreFromSeed(seed, pw, h, { offset });
    else if (mode === 'keys') wallet = await eng.restoreFromKeys(addr, vk, sk, pw, h);
    else if (mode === 'view') wallet = await eng.restoreViewOnly(addr, vk, pw, h);
    else {
      const { keysData } = mode === 'backup' ? eng.parseBackup(await readFile(keysFile)) : { keysData: await readFile(keysFile) };
      const cache = mode === 'desktop' && cacheFile ? await readFile(cacheFile) : undefined;
      try { wallet = await eng.openFiles(keysData, cache, pw); } catch (e) { throw wrongPw(e); }
    }
    await adopt(wallet, true);
  });

  const ready = okPw && (
    mode === 'create' ||
    (mode === 'seed' && seed.trim().split(/\s+/).length === 25) ||
    (mode === 'keys' && isAddress(addr) && isHex64(vk) && isHex64(sk)) ||
    (mode === 'view' && isAddress(addr) && isHex64(vk)) ||
    ((mode === 'backup' || mode === 'desktop') && keysFile));
  const fileInput = (label, onPick, accept) => (
    <p><label className="muted">{label} <input type="file" accept={accept} onChange={(e) => onPick(e.target.files[0] || null)} /></label></p>
  );
  return (
    <div className="card">
      <div className="controls">
        <button className={mode === 'create' ? 'on' : ''} onClick={() => setMode('create')}>Create a wallet</button>
        <button className={mode === 'seed' ? 'on' : ''} onClick={() => setMode('seed')}>Restore from seed</button>
        <button className={mode === 'keys' ? 'on' : ''} onClick={() => setMode('keys')}>Restore from private keys</button>
        <button className={mode === 'backup' ? 'on' : ''} onClick={() => setMode('backup')}>Load backup file</button>
        <button className={mode === 'desktop' ? 'on' : ''} onClick={() => setMode('desktop')}>Import desktop wallet</button>
        <button className={mode === 'view' ? 'on' : ''} onClick={() => setMode('view')}>View-only</button>
      </div>
      {mode === 'create' && <p className="muted">Opens at once. Back it up afterwards (seed words or a backup file): the page reminds you until you do.</p>}
      {mode === 'seed' && (
        <>
          <p><textarea className="wide mono" rows={3} placeholder="25 word seed" value={seed} onChange={(e) => setSeed(e.target.value)} /></p>
          <p className="muted">
            The language (English, Spanish, French, German, Italian, Portuguese, Dutch, Russian, Japanese, Chinese, Esperanto, Lojban) is detected from the words.<br />
            Seed offset (optional passphrase) <input type="password" value={offset} onChange={(e) => setOffset(e.target.value)} />
          </p>
        </>
      )}
      {(mode === 'keys' || mode === 'view') && (
        <>
          {mode === 'view'
            ? <p className="muted">A view-only wallet sees incoming payments (mined rewards too) but cannot spend. It cannot see outgoing payments either, so its balance also counts coins the full wallet has already spent.</p>
            : <p className="muted">The address and both private keys (as the desktop wallet's <code>viewkey</code> and <code>spendkey</code> commands print them). They must belong together.</p>}
          <p><input className="wide mono" placeholder="SM64… primary address" value={addr} onChange={(e) => setAddr(e.target.value)} /></p>
          <p><input className="wide mono" placeholder="private view key (64 hex)" value={vk} onChange={(e) => setVk(e.target.value)} /></p>
          {mode === 'keys' && <p><input className="wide mono" type="password" placeholder="private spend key (64 hex)" value={sk} onChange={(e) => setSk(e.target.value)} /></p>}
        </>
      )}
      {mode === 'backup' && (
        <>
          <p className="muted">A file saved by "Download backup" (sm64coin-wallet-….json), or a raw wallet <code>.keys</code> file. It is read in this browser only.</p>
          {fileInput('Backup file', setKeysFile, '.json,.keys,application/json')}
        </>
      )}
      {mode === 'desktop' && (
        <>
          <p className="muted">The SM64Coin desktop wallet (GUI or CLI) writes <code>&lt;name&gt;.keys</code> and a cache file <code>&lt;name&gt;</code>. Pick the .keys file; the cache is optional (it only saves rescanning). Enter that wallet's own password.</p>
          {fileInput('The .keys file', setKeysFile, '.keys')}
          {fileInput('The cache file (optional, no extension)', setCacheFile, '')}
        </>
      )}
      {(mode === 'seed' || mode === 'keys' || mode === 'view') && (
        <p><label className="muted">Start scanning at block <input value={height} size={8} onChange={(e) => setHeight(e.target.value)} /> (0 is always right, a later block is faster)</label></p>
      )}
      <p>
        <input type="password" placeholder={fresh ? 'password (8+ characters)' : 'the file\'s password'} value={pw} onChange={(e) => setPw(e.target.value)} />{' '}
        {fresh && <input type="password" placeholder="repeat it" value={pw2} onChange={(e) => setPw2(e.target.value)} />}
      </p>
      <button disabled={!ready || busy} onClick={go}>{mode === 'create' ? 'Create' : mode === 'backup' || mode === 'desktop' ? 'Open and save in this browser' : 'Restore'}</button>
      {pkOk && (
        <div className="notice" data-testid="passkey-box">
          <b>Passkey wallet (experimental, optional)</b>
          <p className="muted">Instead of a password, a passkey on this device (synced by your password manager or phone) makes and opens the wallet; the same passkey opens the same wallet on any device, on this website only. Nothing about the passkey is sent anywhere.</p>
          <p className="walleterr">If you delete the passkey, the coins are lost unless you saved the seed words. Export them from the wallet page.</p>
          <p className="muted small" data-testid="passkey-site">The passkey wallet is tied to this site's address, <span className="mono">{window.location.hostname}</span> (gumbo64.github.io is shared by every GitHub Pages site of that account), and the same passkey opens a different wallet on any other site address; to move the wallet elsewhere, export its seed words.</p>
          <button disabled={busy} onClick={pkCreate}>Create passkey wallet (experimental)</button>{' '}
          <button disabled={busy} onClick={pkOpen}>Open with passkey</button>
        </div>
      )}
      <p className="muted">Your keys stay in this browser. The password encrypts the wallet here only; it can't be recovered, but the seed words or a backup file always can.</p>
    </div>
  );
}

function Unlock({ rec, run, adopt, forget, busy }) {
  const pkUnlock = () => run('Opening with the passkey', async () => {
    const k = await pk.openPasskey();
    let wallet;
    try { wallet = await eng.openStored(rec, k.password); } catch { throw new Error('That passkey does not open this wallet (pick the one you made it with).'); }
    await adopt(wallet, false, true);
  });
  const [pw, setPw] = useState('');
  const go = () => run('Unlocking', async () => {
    try { await adopt(await eng.openStored(rec, pw), false); }
    catch (e) { throw new Error(/password/i.test(e.message || '') ? 'Wrong password.' : e.message); }
  });
  return (
    <div className="card">
      <h3>Unlock</h3>
      <p className="mono break">{rec ? short(rec.address) : ''}{rec?.viewOnly ? ' (view-only)' : ''}</p>
      {rec?.passkey && <p><button disabled={busy} onClick={pkUnlock}>Open with passkey</button> <span className="muted">this wallet has no password: it is opened by its passkey</span></p>}
      {!rec?.passkey && <input type="password" autoFocus placeholder="password" value={pw} onChange={(e) => setPw(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && pw && go()} />}{' '}
      {!rec?.passkey && <button disabled={!pw || busy} onClick={go}>Unlock</button>}{' '}
      <button className="linkish" onClick={forget}>forget this wallet</button>
    </div>
  );
}

function Open({ w, passkey, address, viewOnly, snap, progress, seed, setSeed, sync, lock, forget, run, refresh }) {
  const [backed, setBacked] = useState(() => isBackedUp(address));
  const [later, setLater] = useState(false);
  const done = () => { markBackedUp(address); setBacked(true); };
  const downloadBackup = () => run('Preparing the backup', async () => {
    const b = await eng.makeBackup(w);
    download(b.name, b.doc, 'application/json');
    done();
  });
  const downloadKeys = () => run('Preparing the keys file', async () => {
    const b = await eng.makeBackup(w);
    download(b.keysName, b.keys, 'application/octet-stream');
    done();
  });
  const showSeed = async () => {
    if (seed) { setSeed(''); return; }
    setSeed(viewOnly ? 'view-only wallet: no seed' : (await eng.allKeys(w)).seed || 'this wallet has no seed words (use Export keys)');
    if (!viewOnly) done();
  };
  return (
    <>
      {!backed && !later && (
        <div className="card notice">
          <h3>Back up your wallet</h3>
          {passkey && <p className="walleterr">Passkey wallet: it opens with your passkey, which only works on this website. If the passkey is deleted the coins are lost unless you save the seed words.</p>}
          <p>This wallet exists only in this browser. <b>Without a backup, clearing the browser's data (or switching browser or device) loses the coins.</b> Your keys stay here; a backup is a copy you keep yourself.</p>
          <div className="controls">
            <button onClick={showSeed}>{seed ? 'Hide seed words' : 'Show seed words'}</button>
            <button onClick={downloadBackup}>Download backup file</button>
            <button className="linkish" onClick={() => setLater(true)}>remind me later</button>
          </div>
          {seed && <p className="mono seedbox">{seed}</p>}
        </div>
      )}
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
          <button onClick={showSeed}>{seed ? 'Hide seed' : 'Show seed'}</button>
          <button onClick={downloadBackup}>Download backup</button>
          <button onClick={downloadKeys} title="the raw wallet2 .keys file, as the desktop wallet reads it">Download .keys</button>
          <button onClick={lock}>Lock</button>
          <button onClick={forget}>Forget</button>
        </div>
        {seed && <p className="mono seedbox">{seed}</p>}
      </div>
      <ExportKeys w={w} run={run} onShown={done} passkey={passkey} />
      {viewOnly && <p className="muted">View-only: spends made by the full wallet are not visible here, so this balance can be higher than the real one (change from a payment shows up as received).</p>}
      {!viewOnly && <Board w={w} run={run} />}
      <Txs snap={snap} />
      {!viewOnly && <Send w={w} snap={snap} run={run} refresh={refresh} />}
    </>
  );
}

function ExportKeys({ w, run, onShown, passkey }) {
  const [step, setStep] = useState(0); // 0 closed, 1 confirm + password, 2 shown
  const [pw, setPw] = useState('');
  const [k, setK] = useState(null);
  const close = () => { setStep(0); setPw(''); setK(null); };
  const reveal = () => run('Checking the password', async () => {
    if (passkey) { const k = await pk.openPasskey(); await eng.checkPassword(w, k.password); } else await eng.checkPassword(w, pw);
    setK(await eng.allKeys(w)); setPw(''); setStep(2); onShown();
  });
  const row = (label, v, secret) => v ? (
    <p><span className="label">{label}</span><br /><span className={`mono break${secret ? ' seedbox' : ''}`}>{v}</span>{' '}
      <button onClick={() => copy(v)}>Copy</button></p>
  ) : null;
  return (
    <div className="card">
      <h3>Export keys</h3>
      {step === 0 && <button onClick={() => setStep(1)}>Export keys…</button>}
      {step === 1 && (
        <>
          <p className="walleterr">Anyone who sees the spend key or the seed words can spend all of this wallet's coins. Show them only where nobody can look over your shoulder, and never paste them into a website or send them to anyone.</p>
          {!passkey && <><input type="password" autoFocus placeholder="wallet password" value={pw} onChange={(e) => setPw(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && pw && reveal()} />{' '}</>}
          <button disabled={!passkey && !pw} onClick={reveal}>I understand, show the keys</button>{' '}
          <button className="linkish" onClick={close}>cancel</button>
        </>
      )}
      {step === 2 && k && (
        <>
          <p className="walleterr">Secret: whoever has the spend key or the seed can spend your coins.</p>
          {row('Seed words (25)' + (k.seedLanguage ? `, ${k.seedLanguage}` : ''), k.seed, true)}
          {!k.seed && <p className="muted">This wallet has no seed words (it was made from keys that are not derived from a seed, or is view-only).</p>}
          {row('Private spend key', k.spendKey, true)}
          {row('Private view key', k.viewKey, true)}
          {row('Public spend key', k.publicSpendKey)}
          {row('Public view key', k.publicViewKey)}
          {row('Address', k.address)}
          <button onClick={close}>Hide</button>
        </>
      )}
    </div>
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
  const [confirm, setConfirm] = useState(false);
  const join = () => run('Signing and submitting', async () => {
    setConfirm(false);
    const n = name.trim();
    try { localStorage.setItem(NAME_KEY, n); } catch { /* storage unavailable */ }
    await eng.boardJoin(w, n);
    setMsg(`On the board as "${n}". Your node shares it with its peers.`);
  });
  return (
    <div className="card">
      <h3>Leaderboard</h3>
      <p className="walleterr">Joining sends this wallet's private view key to the node. The view key lets the node see every incoming payment to this wallet; it cannot spend.</p>
      <p className="muted">
        Opt in to show your blocks on the <Link to="/leaderboard">Leaderboard</Link>. The entry is signed here with your
        spend key (only you can name this address); to count your blocks, nodes are given your address and secret view key,
        so anyone can then see all incoming payments to this wallet (not spend). Use a dedicated mining wallet. An entry
        cannot be removed once shared, only renamed.
      </p>
      <input placeholder="display name" value={name} maxLength={32} onChange={(e) => { setName(e.target.value); setConfirm(false); }} />{' '}
      {!confirm && <button disabled={!name.trim()} onClick={() => setConfirm(true)}>Join / rename…</button>}
      {confirm && (
        <div className="notice" data-testid="board-confirm">
          Join as <b>{name.trim()}</b>: this sends the wallet's <b>private view key</b> to the node, with the address and the
          signed name. With the view key the node, and every node it shares the entry with, can see all incoming payments
          to this wallet; it cannot spend them. This cannot be undone.<br />
          <button onClick={join}>Send the view key and join</button>{' '}
          <button onClick={() => setConfirm(false)}>Cancel</button>
        </div>
      )}
      {msg && <p>{msg}</p>}
    </div>
  );
}
