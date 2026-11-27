// The address book: wallet2's own, stored (encrypted) inside the wallet's cache file, so it is backed up and moved with the
// wallet and never leaves it. Two backends with one shape, `api` = {list(), add(address, label), edit(index, {address?, label?}),
// remove(index), validate(address)}:
//   web / Android WASM wallet: monero-ts (webApi below: getAddressBookEntries, addAddressBookEntry, editAddressBookEntry,
//   deleteAddressBookEntry);  desktop: wallet-rpc (platform/tauri.js: get_address_book, add_address_book, ...).
// `makeBook(api)` adds what both need before anything is saved: an address of THIS chain (the SM64... prefix and wallet2's own
// network check), a label of up to 120 printable characters, and no second entry for the same address. Pure: addrbook.test.mjs.
export const MAX_LABEL = 120;
export const looksLikeAddress = (a) => /^SM6[a-zA-Z0-9]{90,110}$/.test((a || '').trim());

/** {ok, value, error} for a label (trimmed; empty is allowed: the entry then shows its short address). */
export function checkLabel(label) {
  const v = String(label ?? '').trim();
  // eslint-disable-next-line no-control-regex
  // control characters, and the invisible ones that reorder or hide text (bidi overrides / isolates, zero-width): wallet.rs refuses the same
  if (/[\u0000-\u001f\u007f-\u009f\u200b-\u200f\u202a-\u202e\u2060-\u2069\ufeff]/.test(v)) return { ok: false, value: v, error: 'A label has no control characters.' };
  if ([...v].length > MAX_LABEL) return { ok: false, value: v, error: `A label is up to ${MAX_LABEL} characters.` };
  return { ok: true, value: v, error: '' };
}

/** Entries as {index, address, label}, sorted by label (case-insensitive; unlabelled last), then by index. */
export function normalize(list) {
  return (list || []).map((e) => ({ index: Number(e.index), address: String(e.address || ''), label: String(e.label ?? e.description ?? '') }))
    .sort((a, b) => (!a.label - !b.label) || a.label.toLowerCase().localeCompare(b.label.toLowerCase()) || a.index - b.index);
}
export const findByAddress = (entries, address) => (entries || []).find((e) => e.address === String(address || '').trim());
/** The saved label of an address ('' when unknown). */
export const labelOf = (entries, address) => (findByAddress(entries, address) || {}).label || '';
export const shortAddress = (a) => (a ? `${a.slice(0, 8)}…${a.slice(-6)}` : '');

/** The shared rules over a backend. */
export function makeBook(api) {
  const check = async (address, label, entries, ignoreIndex) => {
    const a = String(address || '').trim();
    if (!looksLikeAddress(a)) throw new Error('This is not an SM64Coin address (it starts with SM6).');
    try { await api.validate(a); } catch { throw new Error('This is not a valid SM64Coin address.'); }
    const l = checkLabel(label);
    if (!l.ok) throw new Error(l.error);
    const dup = findByAddress(entries, a);
    if (dup && dup.index !== ignoreIndex) throw new Error(`Already in the address book${dup.label ? ` as "${dup.label}"` : ''}.`);
    return { address: a, label: l.value };
  };
  return {
    list: async () => normalize(await api.list()),
    async add(address, label) {
      const v = await check(address, label, normalize(await api.list()), -1);
      return api.add(v.address, v.label);
    },
    /** `change`: {address?, label?}: only what is given is changed. */
    async edit(index, change) {
      const entries = normalize(await api.list());
      const cur = entries.find((e) => e.index === index);
      if (!cur) throw new Error('That entry is gone.');
      const v = await check('address' in change ? change.address : cur.address, 'label' in change ? change.label : cur.label, entries, index);
      return api.edit(index, { ...('address' in change ? { address: v.address } : {}), ...('label' in change ? { label: v.label } : {}) });
    },
    remove: (index) => api.remove(index),
  };
}

/**
 * The monero-ts backend. `w` is the open MoneroWalletFull; `validate(address)` throws for an address that is not valid on this
 * chain (engine.js passes MoneroUtils.validateAddress); `onChange()` runs after each write (the page saves the wallet's data).
 */
export function webApi(w, validate, onChange = async () => {}) {
  return {
    validate: async (a) => validate(a),
    async list() {
      const entries = await w.getAddressBookEntries();
      return entries.map((e) => ({ index: e.getIndex(), address: e.getAddress(), label: e.getDescription() || '' }));
    },
    async add(address, label) { const i = await w.addAddressBookEntry(address, label); await onChange(); return i; },
    async edit(index, { address, label }) {
      await w.editAddressBookEntry(index, address !== undefined, address, label !== undefined, label);
      await onChange();
    },
    async remove(index) { await w.deleteAddressBookEntry(index); await onChange(); },
  };
}
