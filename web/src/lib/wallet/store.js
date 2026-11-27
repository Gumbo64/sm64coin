// The browser wallet's persistence: one record in IndexedDB. The keys file and the cache are what
// wallet2 itself writes, already encrypted with the wallet password (ChaCha, key from the password
// by cn_slow_hash): this store never sees a key or a password in the clear.
const DB = 'sm64coin-wallet';
const STORE = 'wallets';
const ID = 'main';

const open = () => new Promise((resolve, reject) => {
  const r = indexedDB.open(DB, 1);
  r.onupgradeneeded = () => r.result.createObjectStore(STORE, { keyPath: 'id' });
  r.onsuccess = () => resolve(r.result);
  r.onerror = () => reject(r.error);
});

const tx = async (mode, fn) => {
  const db = await open();
  try {
    return await new Promise((resolve, reject) => {
      const t = db.transaction(STORE, mode);
      const req = fn(t.objectStore(STORE));
      t.oncomplete = () => resolve(req && req.result);
      t.onerror = () => reject(t.error);
      t.onabort = () => reject(t.error);
    });
  } finally { db.close(); }
};

/** { keysData: Uint8Array, cacheData: Uint8Array, address, viewOnly, updatedAt } or undefined. */
export const loadStored = () => tx('readonly', (s) => s.get(ID));
export const saveStored = (rec) => tx('readwrite', (s) => s.put({ ...rec, id: ID, updatedAt: Date.now() }));
export const forgetStored = () => tx('readwrite', (s) => { s.delete(QID); return s.delete(ID); });

// Quick-unlock (optional): one more record in the same store, {kind: 'pin' | 'android', ...}: the wallet password sealed by a PIN
// (lib/wallet/pin.js) or by the Android Keystore (platform quick unlock). It is never part of the wallet record, so the sync's
// saves cannot drop it; null deletes it.
const QID = 'quick';
export const loadQuick = () => tx('readonly', (s) => s.get(QID));
export const saveQuick = (rec) => tx('readwrite', (s) => (rec ? s.put({ ...rec, id: QID }) : s.delete(QID)));
