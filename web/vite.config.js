import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// The daemon and wallet RPCs don't send CORS headers, so the dev/preview
// server proxies them: /json_rpc -> sm64coin daemon, /wallet/json_rpc ->
// monero-wallet-rpc. Override with SM64COIN_DAEMON / SM64COIN_WALLET.
const daemon = process.env.SM64COIN_DAEMON || 'http://127.0.0.1:16481'
const wallet = process.env.SM64COIN_WALLET || 'http://127.0.0.1:16483'
const leaderboard = process.env.SM64COIN_LEADERBOARD || 'http://127.0.0.1:16490'
const proxy = {
  '/json_rpc': { target: daemon, changeOrigin: true },
  '/wallet/json_rpc': { target: wallet, changeOrigin: true, rewrite: (p) => p.replace(/^\/wallet/, '') },
  '/leaderboard-api': { target: leaderboard, changeOrigin: true, rewrite: (p) => p.replace(/^\/leaderboard-api/, '') },
}

export default defineConfig({
  plugins: [react()],
  server: { proxy, port: 5181, host: true },
  preview: { proxy, port: 4174 },
})
