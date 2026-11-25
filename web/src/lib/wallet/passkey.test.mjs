// The passkey wallet's derivation is FROZEN (see passkey.js): these vectors were computed independently of passkey.js
// (Python: hmac/hashlib HKDF-SHA256, int.from_bytes(.., 'little') % l), so any edit that changes an address fails here.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

// passkey.js imports the node RPC (Vite-only import.meta.env); derive() doesn't use it, so load it with a stub.
const src = readFileSync(new URL('./passkey.js', import.meta.url), 'utf8').replace(/^import \{ daemon \} from '\.\.\/rpc\.js';$/m, 'const daemon = null;');
assert.ok(!/from '\.\.\/rpc\.js'/.test(src), 'the rpc import was not stubbed');
const { derive, SALT_LABEL } = await import(`data:text/javascript,${encodeURIComponent(src)}`);

const hex = (h) => Uint8Array.from(h.match(/../g), (b) => parseInt(b, 16));
const VECTORS = [
  ['000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f',
    '515b9b403f146e26138101a39236d43572403ed7973df89eeac614d775603d09',
    'b0c209957f1080b17bb221e98545c0b7d3e6c0313bfca5c9a23798511d361c04'],
  ['ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
    '09e1127e8abd5332bdcfa5b82d926d2a63f680207bcc44f6e06eeb1773dcd708',
    '35ee67893115c856575a90c5b34c7a8d77677869655b9b38da85b855390dcd29'],
];
for (const [prf, spendKey, password] of VECTORS) assert.deepEqual(await derive(hex(prf)), { spendKey, password });
assert.equal(SALT_LABEL, 'sm64coin-passkey-wallet-v1');
const salt = Buffer.from(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(SALT_LABEL))).toString('hex');
assert.equal(salt, '3dc0e788dd0e8e698d06417957dae993711e03ef4bf46748d3de9ce74715eb55');
console.log('passkey ok');
