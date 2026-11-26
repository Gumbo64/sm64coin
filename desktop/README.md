# SM64Coin desktop app (Tauri 2)

The website (`web/`, React) in a Tauri window, with a Rust backend for what a browser cannot do well: a local full
node, the native miner, (later) wallet-rpc and iroh. The UI is not forked: `web/src/platform/` has two implementations
(`browser`, `tauri`) picked at startup (`window.__TAURI_INTERNALS__`); pages ask `getPlatform()` for `rpc`, `node`,
`nativeMiner`, `rom` and treat null as "not here". Rust is the single source of logic: `pow/launcher` (library +
the `sm64coin` CLI) does ROM search, node start/stop/status, the RPC whitelist and seeds/DHT; `src-tauri` is a thin
command layer over it.

## Build and run (Linux, phase 1)

Needs: Rust, `cargo tauri` 2.x, webkit2gtk-4.1 dev libs, Node. The sidecars are not built here: point
`SM64COIN_BUNDLE` at an unpacked release bundle (sm64coind, sm64coin-automine, rrtrun, routes/, seeds.txt, baserom).

    cd web && npm ci && npx vite build --mode desktop          # -> web/dist-desktop (base '/')
    cd ../desktop/src-tauri && cargo build                       # debug; embeds web/dist-desktop
    SM64COIN_BUNDLE=~/apps/sm64coin-0.5.0-linux-x86_64 ./target/debug/sm64coin-desktop

`web/src/wasm/` (perturb, webminer) and `web/public/wallet/` are build artifacts (scripts/build-wasm.sh,
build-wallet-wasm.sh), needed for `vite build` as for the website.

Environment: `SM64COIN_BUNDLE` (sidecar folder, default: next to the app), `SM64COIN_DATA_DIR` (default `~/.sm64coin`,
shared with the CLI), `SM64COIN_{P2P,RPC,ZMQ}_PORT` (defaults 16480-2; a free port nearby is used if taken, a node
already answering on the RPC port is reused and left running), `SM64COIN_NO_DHT`.

## What exists

Commands (all return `{code, message}` errors; `capabilities/default.json` grants exactly these, plus event listen):
`host_info`, `rom_set(path)`, `node_start/stop/status`, `node_rpc(method, params)` (whitelist `rpc::allowed`, no CORS,
no HTTP from the webview), `miner_start({address, threads})/miner_stop/miner_status`; events `miner-log`, `miner-state`.
The node starts with the app when a ROM is found and is stopped on quit (also on SIGTERM/SIGINT). The miner runs at
low priority against the local node. The Mine page shows a "Native miner" card (threads under the gear, local node
status, log); the WASM auto-miner stays available.

Webview findings (WebKitGTK 2.x): WebGL2 works; WASM works; **SharedArrayBuffer / cross-origin isolation do not**
(`crossOriginIsolated` is false although tauri.conf.json sends COOP/COEP), so the WASM workers run without worker sync
(they search separately). The native miner is the fast path anyway (about 660 k frames/s on 15 threads here). The
CSP also gives workers no `'unsafe-eval'` (unlike deploy/cloudflare/_headers `/workers/*`); the web miner's worker glue
needs it, so check it in the app before relying on the WASM miner there. The webview's wallet engine talks to the local
node directly (node CORS origins include `tauri://localhost`).

## Not done in phase 1 (phase 2)

- Packaging: bundles per OS (deb/AppImage/rpm, msi/nsis, dmg), sidecars via `bundle.externalBin` named per target
  triple (`sm64coind-x86_64-unknown-linux-gnu`, ...) plus resources (routes/, seeds.txt, web is embedded), updater
  plugin + signing keys, Windows code signing, macOS notarization. Release workflow integration (release.yml).
- File dialog (tauri-plugin-dialog) for the ROM instead of typing a path; the web game still asks for its own ROM
  upload in the webview (could be fed from `rom`).
- Fall back to the public node when no local node runs; show node errors prominently; Windows job object for the
  app's own children (launcher `proc` already has it), pdeathsig on Linux.
- iroh group mining (same tree-sync records) in Rust.

### Wallet (sm64coin-wallet-rpc sidecar)

wallet-rpc bound to 127.0.0.1 on a random port with a random per-launch `--rpc-login` kept only in memory; the wallet
directory is mode 0700. Behaviours copied from the Qt GUI: password to open, lock after N idle minutes
(`close_wallet`), password before send (default on) and before showing the seed/keys, hide balance, KDF rounds option.
Passwords and keys are never logged. The webview never sees the login: a `wallet_rpc(method, params)` command with its
own method whitelist forwards.

### "Use my website passkey"

The app opens a one-time 127.0.0.1 listener (random port, random secret, ephemeral X25519 key) and opens the system
browser at `https://sm64coin.pages.dev/desktop-link#port,secret,pubkey`. That page runs the existing, frozen passkey
derivation (`web/src/lib/wallet/passkey.js`, must not be modified), encrypts the result to the app key and POSTs it
(handle Chrome Private Network Access: answer the preflight with `Access-Control-Allow-Private-Network: true`). The app
checks the secret and the Origin, imports into wallet-rpc and closes the listener. Fallback: restore from seed words.

### Android APK (Tauri 2 mobile, same project)

Keep desktop-only backend code (node spawn, sidecars, ROM file paths, wallet-rpc) behind `cfg(desktop)` / a cargo
feature (tauri-build sets `cfg(desktop)` / `cfg(mobile)`); today `src-tauri/src/lib.rs` and `miner.rs` are not gated yet.
The platform layer gets a third shape, `tauri-mobile` (`platform.mobile` is already detected from the user agent; use
Tauri's os plugin when added): remote node via `platform.rpc`, native miner available (the miner built for
aarch64-linux-android, off by default, charging-only later), no local node controls (`platform.node` null), ROM via a
file picker into app storage. Not built in phase 1.
