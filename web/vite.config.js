import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

// The replay viewer (player UI, controls, timeline, resolution options, MP4
// export) is sm64-viewer's: one copy, imported as 'sm64-replay-ui' (its
// src/replay-ui.js). Its own imports of react and the game bindings must
// resolve to this app's copies (two Reacts break hooks, two engines are two
// games), hence the aliases; mp4-muxer comes from the viewer's install.
const here = path.dirname(fileURLToPath(import.meta.url))
const viewer = process.env.SM64_VIEWER || path.resolve(here, '../../sm64-viewer/frontend')

// The daemon RPC doesn't send CORS headers, so the dev/preview server
// proxies it: /json_rpc -> sm64coin daemon (and /leaderboard-api -> a
// leaderboard server). Override with SM64COIN_DAEMON / SM64COIN_LEADERBOARD.
// The site never talks to a wallet: keys stay in the wallet app.
const daemon = process.env.SM64COIN_DAEMON || 'http://127.0.0.1:16481'
const leaderboard = process.env.SM64COIN_LEADERBOARD || 'http://127.0.0.1:16490'
const proxy = {
  '/json_rpc': { target: daemon, changeOrigin: true },
  '/leaderboard-api': { target: leaderboard, changeOrigin: true, rewrite: (p) => p.replace(/^\/leaderboard-api/, '') },
}

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      'sm64-replay-ui': path.join(viewer, 'src/replay-ui.js'),
      'sm64-binds-frontend': path.join(here, 'node_modules/sm64-binds-frontend'),
      'mp4-muxer': path.join(viewer, 'node_modules/mp4-muxer'),
    },
    dedupe: ['react', 'react-dom'],
  },
  server: { proxy, port: 5181, host: true, fs: { allow: [here, viewer] } },
  preview: { proxy, port: 4174 },
})
