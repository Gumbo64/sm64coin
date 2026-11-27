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
`host_info`, `rom_set(path)`, `rom_pick` (system file dialog, the dialog plugin is used from Rust only), `rom_bytes` (feeds the in-page game: no second upload), `remote_rpc(url, method, params)` (the same RPC whitelist over https to the public node), `node_start/stop/status`, `node_rpc(method, params)` (whitelist `rpc::allowed`, no CORS,
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

## Phase 2 (done)

- **ROM**: `rom_pick` opens the system file dialog, verifies the SHA-1, copies into the data dir and starts the node; the web
  `RomGate` copies the app's ROM into the game's IndexedDB (`rom.bytes()`), so the game never asks again.
- **Node fallback**: `lib/rpc.js` reads from the local node while it answers (`watchLocalNode`, 4 s probe) and from the public
  node (`public-node.json`, through `remote_rpc`, https only) when it does not; the Network page and the update banner say which.
- **`cfg(desktop)` / `cfg(mobile)`**: `src/desktop.rs`, `miner.rs`, `wallet.rs`, `link.rs` are desktop only; `lib.rs` + `remote.rs`
  (and `host_info`) are all of the Android shape. The launcher crate has a `desktop` feature (default) for its node/ROM/DHT/CLI
  parts; the app depends on it only for desktop targets, with `tls` for `remote_rpc`. `platform` has the third shape
  `tauri-mobile` (remote node via the public node, no node/miner/rom/wallet objects: the page's own ROM upload and WASM wallet).
  Android was not built (no SDK/target here): `cargo check` with the cfgs flipped compiles the mobile half on the host.
- **Wallet** (`wallet.rs`, `pages/WalletDesktop.jsx`): the design above, as built. The sidecar gets its login through a 0600
  config file that is deleted once it answers (it is not in `ps`), speaks HTTP digest to Rust only, and the wallet dir is 0700.
  `wallet_rpc` whitelists methods; `_password` and `_kdf_rounds` are private params consumed in Rust. Password rules are
  enforced in Rust against a salted hash kept while the wallet is open: password for `query_key` (seed, keys) always, for
  `transfer` when the setting is on; weakening a setting needs the password; `transfer` never relays (the page reviews, then
  `relay_tx`). Auto-lock (default 10 min, 0 = never) stores and closes the wallet; the page reports user activity.
  Amounts are strings above 2^53. NB: `main.jsx` stops keyboard events at `window` (for the game), so `onKeyDown` handlers never
  run in the app (the old Wallet page too): use `<form onSubmit>` for Enter.
- **Use my website passkey** (`link.rs`, `pages/DesktopLink.jsx`, `lib/desktopLink.js`): as designed; X25519 + HKDF-SHA256 +
  AES-256-GCM (WebCrypto has no ChaCha); the page sends the wallet's seed words (made with the WASM engine from the frozen
  passkey.js output), the app restores them with a password chosen in the app. Listener: 127.0.0.1, random port, 5 minutes,
  one good POST, 10 bad ones; Origin, Host and secret checked; answers the PNA preflight. Not testable here with a real passkey
  (no browser/authenticator): the page side was played with Node using the same `desktopLink.js` against the running app.

## Packaging (phase 2b)

`.github/workflows/release.yml`: jobs `web-desktop` (frontend) and `desktop` (matrix: Linux x86_64 .deb + .AppImage, Windows NSIS
.exe, macOS arm64 .dmg) after the bundle jobs; `publish` adds the installers to the draft release. `desktop/stage-sidecars.sh
<unpacked bundle> [triple]` stages `src-tauri/stage/` (sidecars named per triple, only the libraries they import, seeds.txt,
routes/); `src-tauri/bundle/<os>.json` is passed with `tauri build --config` (kept out of tauri.conf.json so a plain
`cargo build` needs no sidecars). Installed layout: sidecars next to the program, `lib/` next to them (the bundle's RUNPATH
`$ORIGIN/lib` works unchanged), seeds.txt and routes/ in `/usr/lib/SM64Coin` (deb, AppImage), the install folder (Windows) or
`Contents/Resources` (macOS); `Dirs::res` finds them. Unsigned. TODO: Windows signing, macOS Developer ID + notarization, updater
plugin + keys, Linux aarch64/.rpm, Windows DLL placement (`stage/dll/*` -> `./`) is untested.

## Not done

- iroh group mining (same tree-sync records) in Rust; native miner on Android; Windows job object for the app's own children is
  in the launcher `proc` (children get CREATE_NO_WINDOW now), pdeathsig on Linux.
- Wallet: sweep, subaddresses, address book, view-only wallets, leaderboard join from the app.
