// The wallet backup file: JSON around wallet2's own password-encrypted .keys bytes (pure: no engine, testable in node).
export const BACKUP_FORMAT = 'sm64coin-wallet-backup';
export const b64 = (u8) => { let s = ''; for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode.apply(null, u8.subarray(i, i + 0x8000)); return btoa(s); };
const unb64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

/** Bytes of a picked file -> { keysData, meta? }: our JSON backup, or a raw wallet2 .keys file. */
export function parseBackup(u8) {
  if (u8[0] === 0x7b) {
    let d;
    try { d = JSON.parse(new TextDecoder().decode(u8)); } catch { throw new Error('This is not a wallet backup file.'); }
    if (d.format !== BACKUP_FORMAT) throw new Error('This JSON is not an SM64Coin wallet backup.');
    if (d.version > 1) throw new Error(`Backup format version ${d.version} is newer than this page understands.`);
    if (d.network && d.network !== 'mainnet') throw new Error(`This backup is for network "${d.network}", not SM64Coin.`);
    return { keysData: unb64(d.keysData), meta: d };
  }
  return { keysData: u8 };
}

