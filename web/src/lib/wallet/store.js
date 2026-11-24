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
export const forgetStored = () => tx('readwrite', (s) => s.delete(ID));
