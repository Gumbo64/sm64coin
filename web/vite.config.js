import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

// The replay player (UI, controls, timeline, MP4 export) is sm64-binds-frontend's
// `/replay` subpath. The package is a file: dependency for now (a symlink to
// sm64-binds/javascript), so its own React must resolve to this app's copy
// (two Reacts break hooks): see `dedupe`.
const here = path.dirname(fileURLToPath(import.meta.url))
const binds = path.resolve(here, '../../sm64-binds/javascript')

// The daemon RPC doesn't send CORS headers, so the dev/preview server
// proxies it: /json_rpc -> sm64coin daemon (the leaderboard is the daemon's
// sm64_board too). Override with SM64COIN_DAEMON.
// The site never talks to a wallet: keys stay in the wallet app.
const daemon = process.env.SM64COIN_DAEMON || 'http://127.0.0.1:16481'
const proxy = {
  '/json_rpc': { target: daemon, changeOrigin: true },
}

// SM64COIN_BASE: the path the site is served under (GitHub Pages: /<repo>/).
export default defineConfig({
  base: process.env.SM64COIN_BASE || '/',
  plugins: [react()],
  resolve: {
    dedupe: ['react', 'react-dom'],
    // The package's src/ (its headless game and loader for the client-side replay worker):
    // its export map only exposes the main entry, which pulls in React components.
    alias: { 'sm64-binds-src': path.resolve(here, 'node_modules/sm64-binds-frontend/src') },
  },
  // Workers' dependencies, bundled at startup (found only when a worker starts, they made the
  // dev server re-optimize and reload the page mid-session).
  optimizeDeps: {
    include: ['sm64-binds-src/scripts/sm64/Sm64Headless.js', 'sm64-binds-src/scripts/headlessWasm.js', 'sm64-binds-src/scripts/fileUpload.js'],
  },
  server: { proxy, port: 5181, host: true, fs: { allow: [here, binds] } },
  preview: { proxy, port: 4174 },
})
