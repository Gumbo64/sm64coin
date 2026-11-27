import React, { useCallback, useEffect, useState } from 'react';
import Identicon from './Identicon.jsx';
import { findByAddress, shortAddress, MAX_LABEL } from '../lib/wallet/addrbook.js';

// The address book of the Wallet page (browser, Android and desktop share it): wallet2's own book, kept inside the wallet file.
// `book` is makeBook(api) of lib/wallet/addrbook.js; the parent owns it (a null book = not ready yet: nothing is shown).

/** The book's entries ([] until loaded) and `reload()`. */
export function useBook(book) {
  const [entries, setEntries] = useState([]);
  const reload = useCallback(async () => {
    if (!book) { setEntries([]); return; }
    try { setEntries(await book.list()); } catch { /* the wallet is closing: keep the last list */ }
  }, [book]);
  useEffect(() => { reload(); }, [reload]);
  return { entries, reload };
}

const copy = (t) => { try { navigator.clipboard && navigator.clipboard.writeText(t); } catch { /* clipboard unavailable */ } };

/** A collapsed card: "Address book (N)"; opened it lists the entries (picture, label, address) with Copy / Rename / Remove, and adds one. */
export function AddressBookCard({ book, entries, reload }) {
  const [open, setOpen] = useState(false);
  const [addr, setAddr] = useState('');
  const [label, setLabel] = useState('');
  const [editing, setEditing] = useState(null); // {index, label}
  const [err, setErr] = useState('');
  if (!book) return null;
  const guard = async (f) => { setErr(''); try { await f(); await reload(); } catch (e) { setErr(e.message || String(e)); } };
  const add = () => guard(async () => { await book.add(addr, label); setAddr(''); setLabel(''); });
  const rename = () => guard(async () => { await book.edit(editing.index, { label: editing.label }); setEditing(null); });
  const remove = (e) => guard(async () => { if (window.confirm(`Remove ${e.label || shortAddress(e.address)} from the address book?`)) await book.remove(e.index); });
  return (
    <div className="card" data-testid="addressbook">
      <button className="linkish" onClick={() => setOpen(!open)}>{open ? '▾' : '▸'} Address book ({entries.length})</button>
      {open && (
        <>
          {err && <p className="walleterr" data-testid="book-error">{err}</p>}
          {entries.length === 0 && <p className="muted small">No saved addresses yet.</p>}
          {entries.map((e) => (
            <div key={e.index} className="bookrow" style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap', margin: '6px 0' }}>
              <Identicon address={e.address} size={24} />
              {editing && editing.index === e.index ? (
                <form onSubmit={(ev) => { ev.preventDefault(); rename(); }} style={{ display: 'contents' }}>
                  <input autoFocus value={editing.label} maxLength={MAX_LABEL} size={14} onChange={(ev) => setEditing({ ...editing, label: ev.target.value })} />
                  <button type="submit">Save</button>
                  <button type="button" className="linkish" onClick={() => setEditing(null)}>cancel</button>
                </form>
              ) : (
                <>
                  <span style={{ minWidth: 0, flex: '1 1 140px' }}>
                    <b>{e.label || <span className="muted">(no label)</span>}</b><br />
                    <span className="mono small muted" title={e.address}>{shortAddress(e.address)}</span>
                  </span>
                  <span className="controls">
                    <button onClick={() => copy(e.address)}>Copy</button>
                    <button onClick={() => setEditing({ index: e.index, label: e.label })}>Rename</button>
                    <button onClick={() => remove(e)}>Remove</button>
                  </span>
                </>
              )}
            </div>
          ))}
          <form onSubmit={(ev) => { ev.preventDefault(); add(); }}>
            <p><input className="wide mono" placeholder="SM64… address" value={addr} onChange={(ev) => setAddr(ev.target.value)} /></p>
            <p><input placeholder="label (optional)" value={label} maxLength={MAX_LABEL} size={20} onChange={(ev) => setLabel(ev.target.value)} />{' '}
              <button type="submit" disabled={!addr.trim()}>Add</button></p>
          </form>
          <p className="muted small">Saved inside the wallet (encrypted with it), so a wallet backup carries it. Only addresses of this chain are kept.</p>
        </>
      )}
    </div>
  );
}

/** Send's picker: choose a saved address (renders nothing without entries). */
export function BookPicker({ entries, onPick }) {
  if (!entries || entries.length === 0) return null;
  return (
    <select aria-label="address book" value="" onChange={(e) => { if (e.target.value) onPick(e.target.value); }}>
      <option value="">Address book…</option>
      {entries.map((e) => <option key={e.index} value={e.address}>{e.label || shortAddress(e.address)}</option>)}
    </select>
  );
}

/** What Send shows under the recipient: the saved label for a known address, else a reminder to check it. */
export function RecipientNote({ entries, to }) {
  const a = (to || '').trim();
  const known = findByAddress(entries, a);
  return (
    <p className="muted small" data-testid="to-preview"><Identicon address={a} size={24} /> {known ? <>saved as <b>{known.label || 'unnamed'}</b>.</> : <>{shortAddress(a)}: check it is the right one.</>}</p>
  );
}

/** After a send: offer to keep the recipient (nothing when it is saved already or the book is not available). */
export function SaveToBook({ book, entries, reload, address, onDone }) {
  const [label, setLabel] = useState('');
  const [err, setErr] = useState('');
  if (!book || !address || findByAddress(entries, address)) return null;
  const save = async () => {
    setErr('');
    try { await book.add(address, label); await reload(); onDone && onDone(); } catch (e) { setErr(e.message || String(e)); }
  };
  return (
    <form className="notice small" data-testid="save-to-book" onSubmit={(e) => { e.preventDefault(); save(); }}>
      <Identicon address={address} size={20} /> Save {shortAddress(address)} to the address book?{' '}
      <input placeholder="label" value={label} maxLength={MAX_LABEL} size={14} onChange={(e) => setLabel(e.target.value)} />{' '}
      <button type="submit">Save</button>{' '}
      <button type="button" className="linkish" onClick={() => onDone && onDone()}>no thanks</button>
      {err && <span className="walleterr"> {err}</span>}
    </form>
  );
}
