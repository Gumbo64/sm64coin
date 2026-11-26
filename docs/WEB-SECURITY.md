# SM64Coin website and gateway: security review

Date 2026-11-26. Scope: `web/` of v0.4 (the live v0.3 site, `sm64coin-fork` branch `hardfork-v0.3`, has the same `coi-sw.js`, `rpc.js`, `passkey.js` design; differences are noted only where relevant). Read-only review: code, one HEAD request each to the site and gateway, DNS. Nothing was probed or load-tested.

Facts used: site `https://gumbo64.github.io/sm64coin/` (GitHub Pages, shared origin `gumbo64.github.io`, no CSP / X-Frame-Options / Permissions-Policy / HSTS header seen; static files carry `access-control-allow-origin: *`); gateway `https://eboy.au` (Cloudflare, `/` answers 404, no HSTS seen, CORS only for Origin `https://gumbo64.github.io`); `p2p.eboy.au` is an unproxied A record to the home IP `1.123.158.39`; no CAA record on `eboy.au`.

## 1. Ranked findings

| # | Severity | Finding |
|---|---|---|
| 1 | High | Shared origin `gumbo64.github.io`: any sibling page can read/replace the wallet store and silently drive the passkey |
| 2 | High (design) | Wallet password/passkey secrecy has no second layer against same-origin script: keys file is offline-brute-forceable, passkey PRF is one click away |
| 3 | Medium | No CSP / frame-ancestors: clickjacking of Send, no script-source lock (partly fixed: meta CSP committed) |
| 4 | Medium | `boardJoin` sends the wallet's secret view key to whichever node is configured |
| 5 | Medium | A user-chosen or compromised node can lie (balances, fee, decoys, templates); Send is not independently checked |
| 6 | Medium | Home IP exposed by `p2p.eboy.au` (DNS-only record), direct DoS / targeting surface |
| 7 | Medium | Origin move would reset `signedSeeds` (equivocation risk) and passkey wallet |
| 8 | Low | Service worker `coi-sw.js`: wide re-signing proxy for every same-scope fetch, no kill switch beyond user "disable sync" |
| 9 | Low | `?address=` URL parameter silently sets the mining payout address (phishing link) |
| 10 | Low | `?e2e` / `localStorage sm64coin.e2e` exposes debug hooks (`window.__sm64miner`, `__miner`, ...) |
| 11 | Low | Supply chain: npm lockfile good; wallet engine and WASM builds pull unpinned/unverified inputs |
| 12 | Low | Gateway hygiene: HSTS, CAA, rate limits, WAF, restricted RPC check |
| 13 | Info | No XSS sinks found; third-party resources: none (only `api.github.com` for the release check) |

### 1. Shared origin (High)

What the code stores on `gumbo64.github.io` (all readable by any other page on that origin, including `https://gumbo64.github.io/anything/` in an iframe or via `window.open` of `/sm64coin/`):

- IndexedDB `sm64coin-wallet`/`wallets`/`main` (`web/src/lib/wallet/store.js:5-9`): wallet2's `keysData` and `cacheData` (password-encrypted), address, `viewOnly`, `passkey` flag. The cache holds the full tx history (privacy). Other DBs: `blockstates.js:15`, `tipstore.js:10`, `replay/client.js:15,197` (public chain data, ROM-derived state).
- localStorage: `sm64coin.node` (rpc.js:23), `sm64coin.nodes`, `sm64coin.address`, `sm64coin.signedSeeds` (bestRun.js:84), settings keys, `sm64coin.e2e`. The uploaded ROM lives in IndexedDB too (`getFile('ROM')`, webminer/worker.js:20).
- Never stored in the clear: spend key, seed, password. Passkey wallet: the password is `hex(HKDF(prf))` and is not stored (passkey.js:41-43).

Scenarios, any page on the origin (a forgotten project, a compromised repo of the same account, a user site `gumbo64.github.io` repo, a fork with a bug, a markdown/Jekyll page with injected script):

1. Node swap: write `localStorage['sm64coin.node']='https://evil'`. The wallet then scans and builds transactions through the attacker node (see 5), and `boardJoin` (4) hands it the view key. No prompt, persists.
2. Wallet record replace/steal: overwrite IndexedDB `main` with an attacker keys file (user later "unlocks" and receives to the attacker's address only if they send, but the Receive address shown would be the attacker's: address-swap). Or copy the record and brute force the password offline (password wallets: minimum 8 characters, `Wallet.jsx:252`).
3. Passkey: any same-origin script can call `navigator.credentials.get({publicKey:{rpId:'gumbo64.github.io', extensions:{prf:{eval:{first:SHA256('sm64coin-passkey-wallet-v1')}}}}})` (salt label is public and fixed, `passkey.js:46,98-124`), get the PRF output after one user-verification prompt, run the public derivation and own the spend key. The browser prompt names only the RP ID `gumbo64.github.io`, which the user associates with this site. The attacker can also open `/sm64coin/` in an iframe and call the page's own code. `userVerification: required` only costs the attacker one biometric tap.
4. Service worker: a sibling project can only register scopes under its own path, so `coi-sw.js` (scope `/sm64coin/`) cannot be taken over by `/other/` pages. Exception: a user-site repo `gumbo64.github.io` (root scope `/`) controls pages not covered by a more specific registration, and can `unregister()` this site's worker if it shares the origin (`getRegistrations()` is origin-wide). A sibling can also unregister it (DoS of worker sync only).
5. `eboy.au` CORS allows exactly Origin `https://gumbo64.github.io`, so a sibling page may call the gateway as if it were the site (rate-limit evasion by the same origin, spam `submit_sm64_proof`).

Fix: the only real fix is a dedicated origin (section 8). Interim: audit that no other Pages project or `gumbo64.github.io` user repo exists or is ever added; keep the account's repos private/archived (archived Pages still serve); turn on 2FA and protected branches; warn in the Wallet UI (already partly done, `Wallet.jsx:261`). Effort: interim 1 hour; migration 2-4 days plus user comms.

### 2. No second layer for secrets (High, design)

Passkey flow is correct cryptographically (PRF never leaves the page, `passkey.js:6-8`) but its security boundary is the origin, so see 1.3. For password wallets the record in IndexedDB is the standard Monero keys file (ChaCha with cn_slow_hash), brute-forceable offline but slow; a strong password is the only protection. Recommendations: raise the minimum, add a strength hint, offer the passkey path only on the dedicated origin, and do not store `cacheData` (history) if the record is considered sensitive (re-scan costs time). Effort: low. The ExportKeys screen reveals seed/spend key after password/passkey re-check (`Wallet.jsx:366-390`): good.

### 3. CSP, XSS surface, clickjacking (Medium)

XSS surface review: no `dangerouslySetInnerHTML`, `innerHTML`, `document.write`, `eval` in `src/`. Node-supplied strings (leaderboard names, errors, `chainNote` containing the node URL, block fields) are rendered as React text, which escapes. `href`s: only constants and `latest.url`, which `updates.js:37-40` restricts to `github.com/Gumbo64/sm64coin/releases/`. URL parameters: `address` (validated by `isAddress`, controller.js:78), `vizmock`, `run`, `legacy_macros`, `e2e` (flags). Two `new Function` uses (`webminer/runtime.js:28`, emscripten glue) run on trusted bundled glue inside workers. Dynamic import of `monero-ts.lib.mjs` is same-origin (`engine.js:30`). Conclusion: no reachable XSS found; the remaining risk is a future one or a compromised dependency, which is what CSP limits.

CSP via `<meta>`: feasible and now committed (commit `b722ccb`, `web/index.html`):

```
default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline';
img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self' data:;
connect-src 'self' https: http://127.0.0.1:* http://localhost:*; worker-src 'self' blob:;
object-src 'none'; base-uri 'none'; form-action 'none'
```

Why these: Vite output has external module scripts only, no inline script; WASM needs `wasm-unsafe-eval` (game, miner, perturbation); inline `style` attributes need `'unsafe-inline'` for styles only; the node URL is user-chosen so `connect-src` must allow any `https:` plus loopback http (a node at `http://192.168.x.x` would be blocked, which is also the right mixed-content answer, see 5). `new Function` runs in dedicated workers, which a document meta CSP does not govern (a worker's policy comes from its own response headers, which Pages cannot set), so `'unsafe-eval'` is not needed in the document.

Verified: `SM64COIN_BASE=/sm64coin/ npx vite build` succeeds; headless Chromium loaded `/`, `/mine`, `/automine`, `/leaderboard`, `/blocks`, `/wallet`, `/network`, `/info` with zero CSP violations and `crossOriginIsolated` true (the service worker still works). Not verified: paths behind the ROM gate (the game itself on the main thread), and the wallet engine (not built here; `wallet/monero-ts.lib.mjs` is a same-origin dynamic import, covered by `'self'`; the wallet worker is a dedicated worker). One emscripten helper in the main bundle contains `new Function(""+Z)` only when passed a string callback; it is not used in normal paths but would throw EvalError under this CSP if a game build ever used it. If a game feature breaks after ROM load, the console says which directive; the first suspect is that helper, and the answer is to add `'unsafe-eval'` rather than drop the CSP. Test with a real ROM before the next release.

Limits of meta CSP: no `frame-ancestors`, `report-uri`/`report-to`, or `sandbox`; it does not stop same-origin siblings (1). Clickjacking: a hostile page can iframe `/sm64coin/wallet` and overlay a decoy; Send needs typing an address plus "Confirm and send" (`Wallet.jsx:442-467`) so a UI-redress attack needs the victim to type, but the wallet may be auto-opened by passkey, so it is exploitable with social engineering. Interim frame-buster (cheap, can be bypassed by `sandbox`ed iframes without `allow-same-origin`, which would also break the page's storage): `if (top !== self) document.body.textContent='Open the site directly'` early in `main.jsx`; recommended, 10 lines, not done here to avoid changing app flow. Real fix: `frame-ancestors 'none'` as a header (section 8).

### 4. View key sent to the node (Medium)

`boardJoin` posts `view_key: await w.getPrivateViewKey()` (engine.js:227) to `sm64_board_join` on the configured node, which may be the public gateway or any user-chosen node. The view key reveals all incoming transactions of the wallet forever. It is by design (the node counts blocks), but: make the UI say so on the button; send it only to the default/public node, never to a node chosen in `sm64coin.node` unless confirmed; longer term replace by the heights evidence (already sent, `engine.js:213-219`) plus a signature, with the node verifying outputs through the public view key of the block's coinbase (needs the node side change). Effort: UI warning 30 minutes; protocol change days.

### 5. Node trust (Medium)

What the wallet does with node data: `monero-ts` wallet2 in a worker scans blocks (`getblocks.bin`) and sees all chain data, so balances come from scanning, not from a node-supplied balance; a lying node can withhold blocks/txs (stale balance, hidden incoming payments, false "unlocked"), serve a fake chain with the right genesis (the check is only block 0 hash `nodechain.js:3`, no PoW/difficulty check by the wallet beyond wallet2's usual), supply malicious decoys via `get_outs` (privacy degradation: decoy selection by node), report a wrong fee/dynamic fee, or just refuse/relay selectively (`relayTx`). A node cannot steal funds from signed transactions (outputs and amounts are fixed client-side; `createTx` with `relay:false` then explicit confirm, `Wallet.jsx:446-450`). Mining: a lying node can hand out templates paying someone else; the miner uses a payout address chosen client side and puts it into `get_block_template`, so only a node ignoring it can redirect, and the resulting block would fail the user's own check only if the page verifies the coinbase (add: compare template coinbase `paysTo` to the intended address before mining; Mine.jsx already tracks `task.paysTo`, `pending` vs `address`).

Tx privacy: whichever node relays sees the sender's IP with each tx and when the wallet polls; the wallet downloads whole blocks so the node does not learn which outputs are yours (good), but `get_outs` reveals the real input among decoys to a hostile node (Monero's known limitation). Cloudflare also sees the traffic. Mixed content: the page is https so browsers block `http://` nodes other than loopback (127.0.0.1, localhost are exempt as potentially trustworthy). The default build node `http://127.0.0.1:16481` works; a LAN `http://` node is blocked and the CSP `connect-src` matches. Chrome's Private Network Access may prompt for loopback from a public site (permission prompt rolling out in 2026): needs a note in docs.

Mitigations: label non-default nodes clearly in the wallet header; require explicit confirmation when `sm64coin.node` differs from default/public (partly there via `nodeNote`); verify the tip against a second node (`public-node.json` plus own) and refuse Send if heights/tips disagree; offer Tor/proxy documentation. Effort: medium (a few days for dual-node check).

### 6. Home IP exposure (Medium)

`p2p.eboy.au` resolves to `1.123.158.39` (DNS only). Anyone sees the home address (TLS names, certificate transparency logs, previous DNS records are not needed). Options, cheapest first: (a) keep it, but put the home router/firewall to allow only TCP 16480 inbound, no other service, no RPC port; (b) a small VPS as seed node that peers only with the home node over a private link (WireGuard) and the home node stops accepting inbound P2P (`--hide-my-port`, outgoing only; monerod's `--add-priority-node VPS`); cost about 4 USD/month; (c) Cloudflare Spectrum TCP proxy (paid; hides IP, adds a hop, works for arbitrary TCP). Cloudflare's free proxy does not carry P2P TCP. Also publish more than one seed so the project is not tied to one home line. Effort: 2-3 hours for (b). Do not also expose RPC through the same IP: the Cloudflare tunnel for RPC is the right design (the restricted RPC is not on a public port), keep `cloudflared` credentials out of the repo.

### 7. Migration hazards found while designing section 8 (Medium)

- `signedSeeds` (bestRun.js:84-107) is the equivocation guard in localStorage: a different origin starts empty, so a miner who moves and re-runs the same seed may sign a second, different run (v0.3/v0.4 equivocation rule). Keys are per-template and throwaway (`minerkey.js`), so this is only the seed-to-run memory. Hence migration needs a quiet period or an export.
- The ROM (IndexedDB) and tip/state caches would need re-upload (ROM) or rebuild.
- Passkey: see section 8.

### 8. Service worker `coi-sw.js` (Low)

- Scope: `/sm64coin/` (`sync.js:14,51,91`); script path under the same prefix, so no `Service-Worker-Allowed` needed; sibling projects cannot register into it, but any same-origin page can unregister it or, with an iframe, read it.
- What it does: every fetch under scope is re-fetched and re-wrapped with COOP `same-origin` and COEP `credentialless|require-corp` (`coi-sw.js:11-26`); for `credentialless`, cross-origin `no-cors` requests are sent without credentials. It also sets `Cross-Origin-Resource-Policy: cross-origin` on responses for `require-corp`. That makes the worker a transparent proxy for node RPC requests, with no logic that alters bodies. Abuse potential: only through a XSS or a modified `coi-sw.js` deployment (supply chain: whoever can push to `main` can ship a worker that sees every request, including wallet RPC).
- Update / kill switch: the browser re-fetches `coi-sw.js` on navigation (at most every 24 h, bypassing HTTP cache for the script itself), `skipWaiting` + `clients.claim` make updates immediate. A bad worker would brick pages (every fetch goes through `respondWith`), and fix means publishing a corrected `coi-sw.js`; there is no remote kill switch for a worker that is itself broken in a way that stops it fetching. The user switch `disableSync` (`sync.js:64-70`) and `localStorage sm64coin.syncDisabled` exist but need the page to load first; if the page does not load, recovery is browser site data clearing. Recommendation: a self-destruct condition: in `coi-sw.js`, on `fetch` error for the navigation request fall back to `fetch(e.request)` unmodified (`.catch(() => fetch(e.request))`), and add a version-pinned kill file (`coi-kill.txt` fetched with `no-store` on activate; if present, `registration.unregister()`). Effort: 1 hour.
- COOP same-origin on every page breaks `window.open` popups that need `opener` and some OAuth flows; none are used. COEP `credentialless` blocks no first-party resource; the Safari `require-corp` branch would block third-party images without CORP, none used. With a real header host (section 9) this worker can be dropped entirely.

### 9. Payout-address URL parameter (Low)

`?address=` is accepted when it is a valid address (`controller.js:78`) and then becomes the payout address used for mining, with no persistent warning that it came from a link. A phishing link makes a visitor mine for the attacker. Fix: show the address and a "from link, confirm" banner, do not store `sm64coin.address` until the user confirms. Effort: 30 minutes.

### 10. e2e hooks (Low)

`window.__sm64miner`, `__miner`, `__mineflow`, `__player`, `__cloud` (`lib/e2e.js`, `controller.js:610`, `Mine.jsx:101-125`) are installed when `?e2e` is in the URL or `localStorage sm64coin.e2e` is set. They expose the controller object (start/stop mining, change the address) to any script; with `?e2e` also read through a malicious link. Impact is the same as XSS-after-the-fact and requires the user to open a crafted link or a same-origin script to set localStorage (already total in case 1), so low. Fix: strip hooks from production builds with `import.meta.env.PROD` (and set `VITE_E2E=1` for the e2e builds). Effort: 30 minutes, but the e2e scripts then need an e2e build.

### 11. Supply chain (Low)

- npm: `package.json` has caret ranges but `package-lock.json` is committed, 136 `integrity` hashes, every `resolved` is `registry.npmjs.org`; CI uses `npm ci` (`pages.yml` build job): good. `sm64-binds-frontend` (own package, `^2.3.3`) is on the public registry: protect the npm account with 2FA and provenance; consider pinning to exact versions. Run `npm audit` before each release (not run here: no network scan was requested).
- GitHub Actions use major tags (`actions/checkout@v5`, `setup-node@v5`, `actions/cache@v4`, `upload-pages-artifact@v3`, `deploy-pages@v4`, `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`) rather than commit SHAs, the usual third-party risk: pin the non-GitHub-owned ones (`dtolnay`, `Swatinem`) to SHAs. Permissions are minimal (`contents: read`, `pages: write`, `id-token: write`); `SM64_AI_BINDS_TOKEN` secret is passed to one step that writes a global git `insteadOf` URL containing the token (`pages.yml`): the token ends up in `~/.gitconfig` of a throwaway runner, acceptable, but scope it read-only to that single repo.
- Pages build pulls: `wasm-pack@0.13.1` via npm (pinned), Rust git dependencies pinned by `rev` (`pow/webminer/Cargo.toml:11-14,34`: good), emsdk by tag, but `monero-ts` is cloned at a `MONERO_TS_REV` and `unbound` downloaded from `nlnetlabs.nl` by `curl | tar` without a checksum (`scripts/build-wallet-wasm.sh:47`): add a sha256 check. The wallet engine is the most security-sensitive artifact (holds keys) and is built from those inputs; publish its sha256 in the release notes and let the Wallet page display the loaded file's hash.
- The Pages artifact deploys on every push to `main` touching listed paths: branch protection on `main` (required review) matters as a deployment gate.
- Third-party resources: none (no fonts, CDN, analytics). The only external request is `https://api.github.com/repos/Gumbo64/sm64coin/releases/latest` (`updates.js:5`), a minor IP/UA leak to GitHub (acceptable; fits `connect-src https:`).

### 12. Gateway / CORS / Cloudflare (Low)

- CORS allows only `https://gumbo64.github.io`, with methods POST, PUT, GET, OPTIONS. Good (not `*`; `scripts/public-node.sh:8` defaults `*` for local nodes, make sure the gateway does not use that default). Note any page on that origin passes (finding 1); the allowlist becomes meaningful only on a dedicated origin. Add the new origin to the allowlist during migration and remove the old one at the end; keep `Vary: Origin`. Drop PUT unless used.
- Cloudflare free-plan items to enable: Always Use HTTPS, HSTS (max-age 6 months then 1 year, includeSubDomains once all subdomains are https; the `p2p.eboy.au` DNS-only record is unaffected), minimum TLS 1.2, Automatic HTTPS Rewrites, Rate Limiting rule on `/json_rpc` and `/submit*` (about 100 requests/min/IP, tighter on `submit_sm64_proof` and `sm64_board_join`), WAF managed rules, Bot Fight Mode off for API paths (it breaks legitimate clients; use a rule instead), a custom rule blocking methods other than GET/POST/OPTIONS, a block on non-allowed RPC method names at the edge (a Transform/Worker allowlist of `get_info`, `get_block*`, `get_transactions`, `send_raw_transaction`, `submit_sm64_proof`, `get_outs`, ... so the origin only receives what the restricted RPC would accept), Authenticated Origin Pulls or tunnel only (already tunnel), and CAA records limiting issuance to Let's Encrypt/Google/Cloudflare CAs. `eboy.au/` answers an empty 404: fine; add `X-Content-Type-Options: nosniff` and `Cache-Control: no-store` for `/json_rpc` responses via a Transform Rule.
- Restricted RPC behind the tunnel: confirm `--restricted-rpc` and that `get_connections`, `stop_daemon`, `pop_blocks`, `set_*` are rejected (the README lists them), and that the tunnel's origin service is `127.0.0.1` only.

## 2. What is already good

The wallet never sends keys, seed or password anywhere except the view key in finding 4; keys live in a Web Worker; the keys file is password-encrypted before IndexedDB; the update link is restricted to the repository; React escaping everywhere; Rust deps pinned by rev; lockfile with integrity; a passkey derivation that is versioned and frozen with a documented scheme.

## 3. Recommended migration to a dedicated origin

Goal: an origin you control, so headers (CSP with `frame-ancestors 'none'`, HSTS, COOP/COEP replacing the service worker, `Permissions-Policy`, `X-Content-Type-Options`, `Referrer-Policy: no-referrer`) apply, and nobody else shares storage or passkey RP ID.

Hosting options: (a) Cloudflare Pages on `sm64coin.pages.dev` or a new domain (static, custom headers via a `_headers` file, free, same account as the gateway); (b) keep GitHub Pages but with a custom domain, which gives a private origin but still no custom headers, so put Cloudflare in front and add headers with a Transform Rule. Recommend (a) with the build unchanged (`SM64COIN_BASE=/`, no 404.html trick needed: `_redirects` `/* /index.html 200`). Gateway on the same registrable domain as the site (`api.eboy.au`) is optional; keep CORS exact-origin.

Headers to set (Cloudflare Pages `_headers`):

```
/*
  Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self' https: http://127.0.0.1:* http://localhost:*; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'
  Strict-Transport-Security: max-age=31536000; includeSubDomains
  Cross-Origin-Opener-Policy: same-origin
  Cross-Origin-Embedder-Policy: credentialless
  Permissions-Policy: camera=(), microphone=(), geolocation=(), publickey-credentials-get=(self), publickey-credentials-create=(self)
  Referrer-Policy: no-referrer
  X-Content-Type-Options: nosniff
```

(Dedicated worker scripts then also carry the CSP and can have `'unsafe-eval'` dropped if `new Function` is replaced; the workers need the COEP header too: send the same headers on `*.js`.) The service worker and its reload step in `autoSync` become dead code on that origin (`isolatedNow()` is already true) and can be removed in a later cleanup.

Passkey consequence and a safe plan without changing the key derivation:

- A passkey is bound to its RP ID. Existing passkeys have RP ID `gumbo64.github.io`; code uses `location.hostname` (`passkey.js:80,101,120`). On `sm64coin.pages.dev` the RP ID would be `sm64coin.pages.dev`; a passkey created there is a different credential with a different PRF output, so the same derivation yields a different wallet. The existing passkey cannot be used from the new origin by default: browsers reject an RP ID that is not a registrable-suffix of the origin.
- Option A (recommended): no RP ID tricks. Keep `https://gumbo64.github.io/sm64coin/` alive permanently as a legacy build (it must stay: that is where the IndexedDB records and passkeys live), with the Wallet page extended by a "Move to the new site" panel: (1) unlock (password or passkey, unchanged flow), (2) re-encrypt: `restoreFromSpendKey(spendKey, newPassword, restoreHeight)` is already in `engine.js` and `exportData`/`makeBackup` produce the JSON backup, so the panel offers "download backup encrypted with a NEW password" (for passkey wallets, the passkey-derived password is unknown to the user, so a re-encrypted copy is required) and shows the 25 seed words (ExportKeys exists), (3) the new origin imports the backup (`parseBackup`, `Wallet.jsx` backup mode) or the seed. After import the user may create a new passkey wallet at the new origin and send the funds there (one transaction; a coin lock of ~1 h `SPENDABLE_AGE`), or keep the imported password wallet. No change to `passkey.js`, the old wallet is untouched until the user chooses. Never transfer secrets by `postMessage` between the origins or in URL fragments.
- Option B (Related Origin Requests, WebAuthn L3): the new origin keeps RP ID `gumbo64.github.io` if `https://gumbo64.github.io/.well-known/webauthn` lists the new origin. Requires a user-site repo `gumbo64.github.io` serving that JSON file (GitHub Pages serves `.well-known` only from the user-site root with Jekyll disabled/`.nojekyll`), browser support (Chrome 128+, Safari 18+; Firefox not at the time of writing, verify), and a one-line change of `rpId` in `passkey.js` (an RP ID constant, not derivation, but the file is frozen, so this needs the maintainer's explicit decision). It keeps old wallets working but also keeps finding 1.3 alive: anything on `gumbo64.github.io` can still ask for the PRF. Not recommended as the end state; acceptable as a temporary bridge only.
- Option C: leave passkeys as is and warn. Cheapest, leaves the main risk.

Migration steps:

1. Now (day 0): merge the meta CSP (done in this branch), add the frame-buster, the `?address=` banner, the e2e strip, the `coi-sw.js` fetch fallback and kill file; set Cloudflare HSTS/rate-limit/CAA; confirm no other Pages project or user-site repo exists on the account.
2. Register the new domain on Cloudflare, deploy the same build (`SM64COIN_BASE=/`) with the headers above; add its origin to the gateway CORS allowlist (both origins during the transition). Keep a 301-free approach: the old site stays fully functional.
3. Ship the "Move to the new site" panel and a banner on the old site for N weeks. The new site shows an "import from the old site" help page. Document that miners must stop mining on the old origin before mining on the new one with the same address and wait one block window (`MAX_RECORDS_WAIT` 120 s plus the proof window) so `signedSeeds` is not needed (or export `sm64coin.signedSeeds` JSON as a non-secret file in the same panel).
4. After the transition: the old origin shows only the legacy wallet (export/restore) page and a link; never delete it, never reuse the repo for anything else; remove the old origin from the CORS allowlist when wallet traffic from it is negligible (read-only export does not need the gateway).
5. On the new origin: no other site shares it (use only `sm64coin.pages.dev` for the app; keep unrelated services on other hostnames, preferably another registrable domain, because cookies/Service Workers/CSP are per-origin but passkey RP IDs relate to registrable suffixes, so a sibling subdomain page cannot use the `sm64coin.pages.dev` RP ID, but `eboy.au` itself could if someone used RP ID `eboy.au`; use RP ID `sm64coin.pages.dev` explicitly, which is what `location.hostname` gives).

Effort: roughly 1 day for hosting, headers and CORS; 1-2 days for the migration panel; 1 day for testing with a real passkey and a real ROM; plus a few weeks of parallel running.

## 4. Priority plan

1. Today: this branch's meta CSP (test with a ROM), then account hygiene on `gumbo64.github.io` (no sibling projects, 2FA), Cloudflare HSTS/rate limits/CAA/method allowlist, view-key warning (4), `?address=` banner (9).
2. This week: dedicated origin with real headers (section 3), `coi-sw.js` fallback/kill switch, e2e hooks stripped from production, frame-buster as interim.
3. Next: migration panel and communication; VPS seed for P2P (6); dual-node tip check and coinbase-payout check (5); pin third-party GitHub Actions and checksum the unbound download (11).

## 5. Implementation status (2026-11-26)

Done in this branch: frame-buster (`lib/security.js`, `main.jsx`), view-key warning naming the node (Wallet, Leaderboard card), `?address=` confirmation banner, e2e hooks stripped from production (`VITE_E2E=1` to keep), `coi-sw.js` fetch fallback and remote kill file, meta CSP kept and checked against the real build, dedicated-origin build (`npm run build:app`, `web/deploy/cloudflare/`), `cloudflare-pages.yml`, the migration panel and importer (`lib/migrate.js`), node CORS defaults for both origins. Operational steps (Cloudflare, DNS, secrets, Pi): `docs/CLOUDFLARE-PAGES.md`. Not done: the VPS seed (6), dual-node tip check (5), action pinning and the `unbound` checksum (11).
