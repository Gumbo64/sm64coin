# Dedicated origin on Cloudflare Pages: setup, CORS, gateway settings

The app now has two builds (see `docs/WEB-SECURITY.md`):

| | legacy | dedicated |
|---|---|---|
| URL | `https://gumbo64.github.io/sm64coin/` (GitHub Pages, `pages.yml`) | `https://app.eboy.au/` (Cloudflare Pages, `cloudflare-pages.yml`) |
| build | `SM64COIN_BASE=/sm64coin/ npm run build` | `npm run build:app` (base `/`, `VITE_DEDICATED=1`, output `web/dist-app`) |
| headers | none (meta CSP, frame-buster, `coi-sw.js`) | `web/deploy/cloudflare/_headers`: CSP with `frame-ancestors 'none'`, HSTS, COOP/COEP `require-corp`, Permissions-Policy, nosniff, no-referrer, X-Frame-Options |
| extras | "moved" banner, "Move to the new site" panel on the Wallet page | no service worker, import help for the miner state |

The legacy site stays forever (wallets and passkeys of existing users live there).

## 1. One-time steps for the maintainer

1. **Cloudflare Pages project (Direct Upload).** Dashboard, Workers & Pages, Create, Pages, "Upload assets" (Direct Upload), project name `sm64coin`, upload any small file (e.g. an empty `index.html`) to create it. (Or: `npx wrangler pages project create sm64coin --production-branch main`.) If you pick another name, set the repository variable `CF_PAGES_PROJECT` to it.
2. **Custom domain.** In that project: Custom domains, Set up a custom domain, `app.eboy.au`. `eboy.au` is on Cloudflare, so the CNAME `app` to `sm64coin.pages.dev` (proxied) is created for you; confirm in DNS that it exists and is orange (proxied). Wait until the domain shows "Active".
3. **API token.** My Profile, API Tokens, Create Token, Custom token: Permissions `Account` / `Cloudflare Pages` / `Edit`, Account Resources: your account. Copy the token.
4. **Account ID.** Dashboard, any zone or Workers & Pages overview, right sidebar "Account ID".
5. **GitHub secrets.** Repository Settings, Secrets and variables, Actions: new repository secrets `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` (and the existing `SM64_AI_BINDS_TOKEN`). Optional variable `CF_PAGES_PROJECT`. Without the two secrets `cloudflare-pages.yml` finishes green after its first job, doing nothing.
6. **First deploy.** Actions, "Cloudflare Pages", Run workflow (branch `main`). Afterwards every push to `main` that touches the site deploys.
7. **Check.** `curl -sI https://app.eboy.au/ | grep -i "content-security\|strict-transport\|cross-origin"` shows the headers; open the site: DevTools Console must show no CSP violations; `crossOriginIsolated` is `true` (the Auto-mine card says worker sync is on, no reload).
8. **The Pi's CORS list** (section 2) must be changed first, or the new site cannot call the node.
9. **Cloudflare zone settings for `eboy.au`** (section 3).
10. Optional: Pages project, Settings, "Production" / "Preview" : turn off preview deployments if you do not use them. The `*.pages.dev` address serves the same app on another origin (its wallets are separate); the gateway CORS does not allow it.

## 2. Pi node CORS (both origins during the transition)

monerod's `--rpc-access-control-origins` takes a **comma separated list** of origins; it answers `Access-Control-Allow-Origin` with the request's origin when it is in the list. `scripts/sm64coin-node`, `scripts/start-node.sh` and the desktop launcher now default to both origins. On the Pi (`sm64coin-node.service`, node script reads `ORIGINS`):

```
sudo systemctl edit sm64coin-node
```
```
[Service]
Environment=ORIGINS=https://gumbo64.github.io,https://app.eboy.au
```
```
sudo systemctl restart sm64coin-node
curl -si -X OPTIONS https://eboy.au/json_rpc -H 'Origin: https://app.eboy.au' -H 'Access-Control-Request-Method: POST' | grep -i access-control-allow-origin
curl -si -X OPTIONS https://eboy.au/json_rpc -H 'Origin: https://gumbo64.github.io' -H 'Access-Control-Request-Method: POST' | grep -i access-control-allow-origin
curl -si -X OPTIONS https://eboy.au/json_rpc -H 'Origin: https://evil.example' -H 'Access-Control-Request-Method: POST' | grep -ci access-control-allow-origin   # 0
```
If the Pi uses a hand-written unit with `--rpc-access-control-origins` (`docs/raspberry-pi.md` shows `*`; the gateway must not use `*`), put the same list there. Never use `*` on the public gateway. When wallet traffic from the old origin is negligible (the Move panel needs no gateway), remove `https://gumbo64.github.io` from the list.

## 3. Cloudflare settings for the gateway zone (`eboy.au`)

- SSL/TLS: mode Full (strict); Always Use HTTPS on; Minimum TLS 1.2; Automatic HTTPS Rewrites on.
- HSTS (SSL/TLS, Edge Certificates): enable, max-age 6 months first (15552000), then 12 months; include subdomains only after checking that every subdomain that browsers open is https (`p2p.eboy.au` is a DNS-only TCP endpoint, unaffected); no preload yet. The Pages site sets its own HSTS via `_headers`.
- DNS: CAA records `0 issue "letsencrypt.org"`, `0 issue "pki.goog"`, `0 issue "digicert.com"` (Cloudflare's universal certificate CAs; check the current list in the Cloudflare docs before adding), `0 issuewild ";"` if you use no wildcard, and `0 iodef "mailto:..."`.
- Security, WAF, Custom rules:
  - Block methods the gateway does not need (drops PUT): `(http.host eq "eboy.au" and not http.request.method in {"GET" "POST" "OPTIONS"})`, action Block.
  - Rate limiting (free plan: one rule, 10 s window): `(http.host eq "eboy.au" and http.request.uri.path eq "/json_rpc")` about 100 requests per 10 s per IP... tune from the logs; a tighter second rule where the plan allows it for `submit_sm64_proof` and `sm64_board_join` (these need body matching, a paid-plan feature; otherwise rely on the node's own limits).
  - Leave Bot Fight Mode off for `eboy.au` (it breaks API clients); use rules instead.
- Transform Rules, Modify Response Header for `eboy.au`: `X-Content-Type-Options: nosniff`; `Cache-Control: no-store` when path is `/json_rpc`.
- RPC method allowlist at the edge (so the origin only sees what the restricted RPC would accept anyway): needs body inspection, so a small Cloudflare Worker in front of the tunnel (allow `get_info`, `get_block*`, `get_transactions`, `send_raw_transaction`, `submit_sm64_proof`, `sm64_*`, `get_outs`, the binary `.bin` endpoints); optional, the node's `--restricted-rpc` already refuses the dangerous methods.
- Zero Trust tunnel: origin service `http://127.0.0.1:<rpc port>`; keep the tunnel credentials out of the repo.

## 4. Kill switch of the legacy `coi-sw.js`

Publish a file `web/public/coi-sw-off.txt` whose first word is `off` (it is deployed at `/sm64coin/coi-sw-off.txt`). Open pages stop using the worker within 10 minutes and unregister it; the page also checks the file before registering a worker. Delete the file to re-enable. (A file that does not start with `off`, such as an `index.html` fallback, is ignored.)

## 5. Testing the build locally

```
cd web
npm run build:app                      # web/dist-app (with _headers, _redirects)
node src/lib/security.test.mjs         # meta CSP == header CSP, coi-sw kill file and fallback
```
E2E hooks (`window.__sm64miner`, ...) exist only in dev and in builds made with `VITE_E2E=1`; the e2e scripts must build that way (`VITE_E2E=1 npm run build`) and open the page with `?e2e`.
