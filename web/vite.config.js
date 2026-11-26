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
// Dev switch: SM64_BINDS_DIR=/path/to/sm64-binds/javascript uses that checkout's package
// instead of node_modules/sm64-binds-frontend (to try unpublished sm64-binds changes).
const bindsDir = process.env.SM64_BINDS_DIR ? path.resolve(process.env.SM64_BINDS_DIR) : null

// The daemon RPC doesn't send CORS headers, so the dev/preview server
// proxies it: /json_rpc -> sm64coin daemon (the leaderboard is the daemon's
// sm64_board too). Override with SM64COIN_DAEMON.
// The site never talks to a wallet: keys stay in the wallet app.
const daemon = process.env.SM64COIN_DAEMON || 'http://127.0.0.1:16481'
// The wallet (a wasm wallet2 in a web worker) also calls the daemon's other RPC endpoints (binary
// getblocks, get_outs, sendrawtransaction ...); a node that isn't this site's proxy must allow CORS for them.
const walletRpc = '^/(get_?blocks(_by_height)?\\.bin|get_?hashes\\.bin|get_o_indexes\\.bin|get_outs(\\.bin)?|get_?height|get_?info|get_transactions|gettransactions|send_?raw_?transaction|is_key_image_spent|get_transaction_pool[a-z_.]*|get_output_distribution(\\.bin)?)$'
const proxy = {
  '/json_rpc': { target: daemon, changeOrigin: true },
  [walletRpc]: { target: daemon, changeOrigin: true },
}

// SM64COIN_BASE: the path the site is served under (GitHub Pages: /<repo>/).
export default defineConfig(({ mode }) => ({
  base: process.env.SM64COIN_BASE || '/',
  plugins: [react()],
  // Worker bundles in their own folder: deploy/cloudflare/_headers gives /workers/* (the wasm-bindgen glue is built with
  // new Function in a worker) a CSP that allows it, and the document and everything else the strict one.
  worker: { rollupOptions: { output: { entryFileNames: 'workers/[name]-[hash].js', chunkFileNames: 'workers/[name]-[hash].js' } } },
  // `--mode desktop` builds the Tauri app's frontend (desktop/src-tauri/tauri.conf.json frontendDist).
  build: { outDir: process.env.SM64COIN_OUTDIR || (mode === 'desktop' ? 'dist-desktop' : 'dist') },
  resolve: {
    dedupe: ['react', 'react-dom'],
    // The package's src/ (its headless game and loader for the client-side replay worker):
    // its export map only exposes the main entry, which pulls in React components.
    alias: bindsDir
      ? [
          { find: /^sm64-binds-frontend$/, replacement: path.join(bindsDir, 'index.js') },
          { find: /^sm64-binds-frontend\/replay$/, replacement: path.join(bindsDir, 'src/replay/index.js') },
          { find: /^sm64-binds-frontend\/replay\.css$/, replacement: path.join(bindsDir, 'src/replay/replay.css') },
          { find: 'sm64-binds-src', replacement: path.join(bindsDir, 'src') },
        ]
      : { 'sm64-binds-src': path.resolve(here, 'node_modules/sm64-binds-frontend/src') },
  },
  // Workers' dependencies, bundled at startup (found only when a worker starts, they made the
  // dev server re-optimize and reload the page mid-session).
  optimizeDeps: {
    include: ['sm64-binds-src/scripts/sm64/Sm64Headless.js', 'sm64-binds-src/scripts/headlessWasm.js', 'sm64-binds-src/scripts/fileUpload.js'],
  },
  server: { proxy, port: 5181, host: true, fs: { allow: [here, binds, ...(bindsDir ? [bindsDir] : [])] } },
  preview: { proxy, port: 4174 },
}))
