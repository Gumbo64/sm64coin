import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

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
  server: { proxy, port: 5181, host: true },
  preview: { proxy, port: 4174 },
})
