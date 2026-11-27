// node --test src/lib/desktopLink.test.mjs : the page side of the hand-off (the Rust side has its own round-trip test).
import test from 'node:test';
import assert from 'node:assert/strict';
import { parseLink, seal, b64u, unb64u } from './desktopLink.js';

test('parseLink accepts the app link and refuses junk', () => {
  const k = b64u(new Uint8Array(32).fill(5));
  const ok = parseLink(`#p=40123&s=${'a'.repeat(22)}&k=${k}`);
  assert.equal(ok.port, 40123);
  assert.equal(ok.appKey.length, 32);
  for (const bad of ['', '#p=1&s=abc&k=zz', `#p=80&s=${'a'.repeat(22)}&k=${k}`, `#p=40123&s=${'a'.repeat(22)}&k=${b64u(new Uint8Array(31))}`, `#p=40123&s=short&k=${k}`]) assert.equal(parseLink(bad), null, bad);
});

test('seal: the app key opens it (WebCrypto both sides), another key does not', async () => {
  const { subtle } = globalThis.crypto;
  const app = await subtle.generateKey({ name: 'X25519' }, true, ['deriveBits']);
  const appPk = new Uint8Array(await subtle.exportKey('raw', app.publicKey));
  const plain = { seed: 'one two three', height: 7, address: 'SM64x' };
  const { pk, iv, ct } = await seal(appPk, plain);
  const open = async (priv) => {
    const theirs = await subtle.importKey('raw', unb64u(pk), { name: 'X25519' }, false, []);
    const shared = await subtle.deriveBits({ name: 'X25519', public: theirs }, priv, 256);
    const salt = new Uint8Array(64); salt.set(appPk, 0); salt.set(unb64u(pk), 32);
    const hk = await subtle.importKey('raw', shared, 'HKDF', false, ['deriveBits']);
    const info = new TextEncoder().encode('sm64coin desktop-link v1');
    const key = await subtle.importKey('raw', await subtle.deriveBits({ name: 'HKDF', hash: 'SHA-256', salt, info }, hk, 256), 'AES-GCM', false, ['decrypt']);
    return JSON.parse(new TextDecoder().decode(await subtle.decrypt({ name: 'AES-GCM', iv: unb64u(iv), additionalData: info }, key, unb64u(ct))));
  };
  assert.deepEqual(await open(app.privateKey), plain);
  const other = await subtle.generateKey({ name: 'X25519' }, true, ['deriveBits']);
  await assert.rejects(open(other.privateKey));
});
