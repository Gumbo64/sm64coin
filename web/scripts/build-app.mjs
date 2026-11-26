// Builds the dedicated-origin site (Cloudflare Pages) into web/dist-app: base '/', VITE_DEDICATED=1 (real headers, no coi-sw.js,
// no migration panel), plus _headers and _redirects. Env passes through (VITE_DEFAULT_NODE, VITE_GIT_SHA, VITE_SM64COIN_VERSION).
import { spawnSync } from 'node:child_process';
import { cpSync, rmSync, existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const web = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const out = path.join(web, 'dist-app');
rmSync(out, { recursive: true, force: true });
const r = spawnSync(process.execPath, [path.join(web, 'node_modules/vite/bin/vite.js'), 'build'], {
  cwd: web, stdio: 'inherit',
  env: { SM64COIN_BASE: '/', SM64COIN_OUTDIR: 'dist-app', VITE_DEFAULT_NODE: "http://127.0.0.1:16481", ...process.env, VITE_DEDICATED: '1' },
});
if (r.status !== 0) process.exit(r.status ?? 1);
for (const f of ['_headers', '_redirects']) cpSync(path.join(web, 'deploy/cloudflare', f), path.join(out, f));
rmSync(path.join(out, 'coi-sw.js'), { force: true }); // the real headers isolate the page
if (!existsSync(path.join(out, 'index.html'))) { console.error('build-app: no index.html'); process.exit(1); }
console.log('dist-app ready');
