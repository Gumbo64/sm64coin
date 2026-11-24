// sync-check.mjs: the built wallet engine (web/public/wallet) syncs the real chain in headless Chromium.
//   node sync-check.mjs [NODE=http://127.0.0.1:29781] [PORT=5351]
// Serves web/public/wallet itself and proxies everything else to the node (same origin, no CORS), opens a
// fresh wallet (restore height 0) with monero-ts in the page and waits until it reaches the node's height:
// wallet2 hashes every block (sm64coin::proof_id from the wasm) and checks the parent links, so a
// wrong block id stops the sync. Needs playwright-core and a Chromium (CHROME, PLAYWRIGHT).
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const here = path.dirname(fileURLToPath(import.meta.url));
const wallet = path.join(here, '../../public/wallet');
const node = new URL(process.argv[2] || 'http://127.0.0.1:29781');
const port = +(process.argv[3] || 5351);
const { chromium } = await import(process.env.PLAYWRIGHT || '/workspaces/sm64-combined/sm64-viewer/frontend/node_modules/playwright-core/index.mjs');
const exe = process.env.CHROME || `${process.env.HOME}/.cache/ms-playwright/chromium-1140/chrome-linux/chrome`;

const server = http.createServer((req, res) => {
  const u = new URL(req.url, 'http://x');
  if (u.pathname === '/') { res.setHeader('content-type', 'text/html'); return res.end('<!doctype html><title>t</title>'); }
  if (u.pathname.startsWith('/wallet/')) {
    const f = path.join(wallet, path.basename(u.pathname));
    if (!fs.existsSync(f)) { res.statusCode = 404; return res.end(); }
    res.setHeader('content-type', f.endsWith('.mjs') || f.endsWith('.js') ? 'text/javascript' : 'application/octet-stream');
    return fs.createReadStream(f).pipe(res);
  }
  if (process.env.VERBOSE) console.log('proxy', req.method, req.url);
  const p = http.request({ host: node.hostname, port: node.port, path: req.url, method: req.method, headers: { ...req.headers, host: node.host } }, (r) => { res.writeHead(r.statusCode, r.headers); r.pipe(res); });
  p.on('error', () => { res.statusCode = 502; res.end(); });
  req.pipe(p);
});
await new Promise((r) => server.listen(port, '127.0.0.1', r));
console.log('launching');
const browser = await chromium.launch({ executablePath: exe, args: ['--no-sandbox', '--disable-gpu', '--disable-dev-shm-usage', '--no-zygote'] });
let code = 1;
try {
  const page = await browser.newPage();
  page.setDefaultTimeout(120000);
  page.on('worker', () => console.log('worker started'));
  page.on('requestfailed', (r) => console.log('reqfailed', r.url()));
  page.on('pageerror', (e) => console.log('pageerror:', String(e).slice(0, 300)));
  page.on('console', (m) => { if (process.env.VERBOSE || m.type() === 'error') console.log('console.error:', m.text().slice(0, 300)); });
  console.log('goto');
  await page.goto(`http://127.0.0.1:${port}/`);
  console.log('evaluate');
  const out = await page.evaluate(async () => {
    console.log('step: import');
    const m = await import(`${location.origin}/wallet/monero-ts.lib.mjs`);
    m.LibraryUtils.setWorkerDistPath(`${location.origin}/wallet/monero.worker.js`);
    console.log('step: createWallet');
    const w = await m.MoneroWalletFull.createWallet(new m.MoneroWalletConfig({ networkType: m.MoneroNetworkType.MAINNET, server: { uri: location.origin }, proxyToWorker: true, password: 'x', language: 'English'}));
    await w.setRestoreHeight(0);
    console.log('step: created');
    const target = await w.getDaemonHeight();
    console.log('step: sync from ' + target);
    await w.sync();
    const h = await w.getHeight();
    const blk = await w.getDaemonMaxPeerHeight().catch(() => null);
    return { target, height: h, peer: blk };
  });
  console.log('wallet synced:', JSON.stringify(out));
  code = out.height >= out.target ? 0 : 1;
  console.log(code === 0 ? 'OK: wallet reached the chain height' : 'FAIL: wallet stopped short of the chain');
} catch (e) {
  console.log('FAIL:', String(e).slice(0, 500));
} finally {
  await browser.close();
  server.close();
}
process.exit(code);
