## SM64Coin @TAG@

**Download, put your ROM next to it, open the wallet.**

1. Download the bundle for your computer:
   - `sm64coin-@VERSION@-linux-x86_64.tar.gz`: Linux on a PC (Intel/AMD, 64-bit)
   - `sm64coin-@VERSION@-linux-aarch64.tar.gz`: Linux on ARM, e.g. Raspberry Pi 4/5 with a 64-bit OS
2. Unpack it (`tar xzf sm64coin-*.tar.gz`) and open the folder.
3. Copy your own US Super Mario 64 ROM into it, named `baserom.us.z64`
   (SHA-1 `9bef1128717f958171a4afac3ed78ee2bb4e86ce`; it is not included).
4. Open the wallet app (`./sm64coin-wallet-gui`; macOS: `SM64Coin-Wallet.app`; Windows: `sm64coin-wallet-gui.exe`).
   The first start creates or restores a wallet and starts its own node, which syncs from the seed nodes.
   Mining page: **Start** runs the automated miner, **Play to mine** opens the website (served by the wallet app itself).

**Servers** (no wallet app): `./sm64coin-node` runs `sm64coind` with the seeds and the ROM; `sm64coin-node.service`
is a systemd unit. Open TCP 16480. Details in `README.txt`.

Nothing else needs to be installed. Verify a download with `sha256sum -c SHA256SUMS --ignore-missing`.
The `sm64coin` launcher is no longer shipped.


### 0.5.7: fix for the website's auto-miner

Same chain as 0.5.6, upgrade in place. Only a fix:

- **The website's auto-miner no longer crashes.** A browser worker died ("unreachable executed …
  `Instant::now`") whenever its search reached the goal, then restarted, so web auto-mining could not
  finish a step. The cause was a call to the standard clock, which does not exist in browsers; the miner
  now uses the browser's clock, and a test keeps the standard clock out of code the browser runs.
  The desktop app, the Android app and the native miner were not affected.

### 0.5.6: the final release (instant blocks from public nodes again, PIN unlock and an address book)

This is the last planned release of SM64Coin. Same chain as 0.5.0 to 0.5.5: no reset, upgrade in place.

- **Tip stream over HTTPS on its own port**: `sm64coin-lc serve --tls-cert FILE --tls-key FILE` serves the tip stream
  (`GET /lc/tip`) as HTTPS straight to the public (default `[::]:16490`), so a node without a reverse proxy (a Raspberry
  Pi behind a router forward) pushes new blocks to browsers and apps again. The certificate files (certbot's) are reloaded
  when they change (and on SIGHUP): a renewal needs no restart. Per-IP caps count the real client address.
- **`tips` in the bootstrap list**: `public-node.json` may say where a node's stream is, `"tips": {"https://eboy.au":
  "https://eboy.au:16490/lc/tip"}` (its `nodes` stay plain strings, which older releases need). The website and the apps
  open the stream there; a node without one keeps the `<node>/lc/tip` attempt behind its reverse proxy. An unreachable tip
  address means polling, never a strike against the node.
- **Wallet: optional PIN / fingerprint unlock** (off unless you turn it on from the wallet page: "Set up PIN", on Android "Set up
  PIN / fingerprint"; nothing asks for it when a wallet is created or restored, and the one-click wallet is unchanged).
  - **Android** (a preview: not yet tried on real phones): the phone's fingerprint (or its screen lock PIN/pattern, Android
    11+) unlocks the wallet. A hardware-backed Android Keystore key that needs your authentication for every use seals the
    wallet password; it stops working when fingerprints or the screen lock change (the password then opens the wallet).
  - **Website and desktop**: a 4 to 8 digit PIN seals the wallet password (PBKDF2, 600 000 rounds). 5 wrong PINs wipe it and the
    password is needed again. A PIN is a convenience lock: someone with this device's files could guess it; use a password for
    strong protection. On the desktop the sealed password lives in the app's data folder (mode 0600) and never reaches the page.
  - A wallet without a password gets a long random one when quick unlock is turned on (so what is stored becomes encrypted;
    back up the seed words first, they open the wallet if the PIN is wiped); turning it off asks for a password of your choice
    or none. The PIN or fingerprint is also accepted where the password is asked (show seed and keys, send, weakening a setting).
- **Wallet: address book** (wallet2's own, kept encrypted inside the wallet file, so a backup carries it): save addresses with a
  label and their picture on the Wallet page, pick one in Send, and "Save to address book" is offered after a send. Only
  addresses of this chain are accepted.

### 0.5.5: the light-client network

Same chain as 0.5.0 to 0.5.4: no reset, upgrade in place.

- **Mine page**: one **Auto-mine** card under the search cloud (it drives the native miner in the apps and the web workers in the
  browser), and a **star board** between them showing which of the 120 stars the block's start state has and which are left,
  with the cap switches and Bowser keys.

- **No single public node any more**: the website, the Android app and the desktop app (while its own node is down)
  keep a scored list of nodes (the release's bootstrap list plus nodes they learn), move to the next one when a node is
  slow or down, and send blocks, proofs and transactions to three nodes at once. The wallet and the native miner talk to a
  loopback daemon in the app that does this for them.
- **Plain HTTP(S) everywhere**: the network is ordinary HTTPS to nodes' RPC (the apps also take plain `http://ip:port`
  nodes), nothing else is needed. The apps find more nodes by asking the nodes they use for Monero's list of public nodes
  (`get_public_nodes`, public addresses only); the website uses the https nodes of its bootstrap list.
- **Tip push**: a node's `sm64coin-lc` service pushes the chain tip to the apps and the website as Server-Sent Events
  (`GET /lc/tip`), so a new block is seen at once and the frame limit is computed on the device from the tip and the
  clock, with a 30 s poll as the fallback. A node behind Caddy or nginx forwards `/lc/tip` with buffering off.
- **Verification**: a light client no longer trusts the node it reads from. It checks the chain from a checkpoint compiled
  into the release: every block's id, links, timestamps, difficulty, frame limit, the miner's signature and the proof
  weight, across several nodes, and follows the heaviest valid chain; a node that serves an invalid chain is banned for a
  week. After the first check only new blocks are fetched. The desktop app also replays the current cycle's Mario runs.
  The status light and the Network page say `verified (n nodes agree)`, `conflict` or `unverified`. If no node can be
  verified the app keeps working and says the reads are unverified.
- **In the browser**: the same Rust code runs as WASM, loaded after the page appears; until it is ready, or if it fails,
  the site uses the public node directly as before.
- **For node runners: `sm64coin-lc`** (in the bundles; Linux also `sm64coin-lc.service`): the light-client service next
  to your node. It reads the node's restricted RPC on 127.0.0.1 (set `RESTRICTED_PORT=16489` on the node unit) and serves
  the tip stream on a local port for your reverse proxy. The launcher has the same as `--lc` (`--lc-sse-bind`).
- **Privacy**: the apps and the website publish nothing about themselves (no DHT or lookup record of your device) and
  only dial nodes they know; the wallet still downloads whole blocks and scans them locally, as with any Monero remote
  node, so a node learns your IP and timing but not which outputs are yours. Joining the leaderboard (which sends your
  view key) only goes to the node you chose, never to a node picked by failover.
- Still to come: the Raspberry Pi in the bootstrap list, then removing the Cloudflare tunnel in front of it.

### 0.5.4: native mining on Android (preview)

Same chain as 0.5.0 to 0.5.3: no reset, upgrade in place.

- **Android app: native mining** (preview). The Mine page's **Auto-mine** card drives the miner inside the app
  (the same search the desktop miner uses), mines against the public node with the ROM you already loaded in the app,
  and feeds the page's visualisations. It is off until you start it; threads default to cores - 1, at most 4. It uses a
  lot of battery and warms the phone, so use it while charging. It mines only while the app is open on screen and stops
  when you leave it (no background service yet). Not yet tested on real phones: please report how it runs.
- **One mining loop on desktop**: the miner is now a library; `sm64coin-automine` is a thin command line over it and the
  search steps run as threads of one process (`--process` keeps the old one-process-per-step mode).
- **Removed**: the experimental route/beam controllers and the tracker (they were not used by the miner).

### 0.5.3: the SM64Coin Android app (preview)

Same chain as 0.5.0 to 0.5.2: no reset, upgrade in place.

- **New: the SM64Coin Android app** (preview): `sm64coin-@VERSION@-android.apk`, for arm64 phones with Android 7 or
  newer. It is the website in an app: it reads the chain from the public node, uses the website's wallet (create or
  restore from seed words; passkey wallets are not available in the app, a website passkey wallet opens from its seed
  words) and your own ROM picked from the phone, and you play to mine with the touch controls. No auto-mining in the
  app yet: native mining on Android comes next.
- Install it with [Obtainium](https://obtainium.imranr.dev/) (**[Add to Obtainium](https://apps.obtainium.imranr.dev/redirect?r=obtainium://add/https://github.com/Gumbo64/sm64coin)**,
  which also keeps it up to date) or download the APK and open it (allow "install unknown apps" for your browser).
  This is the first release of the app id `io.github.gumbo64.sm64coin`. It is signed with the SM64Coin release key,
  certificate SHA-256 `63:10:2D:9D:EE:E7:3F:03:D2:CF:EB:6A:61:E6:77:B1:4E:2A:4B:92:55:60:D2:8B:D5:53:ED:42:4E:45:9A:C4`
  (check with `apksigner verify --print-certs`); updates are only ever signed with that key.

### 0.5.2: the native miner drives the desktop app's views

Same chain as 0.5.0 and 0.5.1: no reset, upgrade in place.

- **Desktop app: the native miner now feeds the Mine page's visualisations** (the 3D search cloud, the strips, the
  highlight reel) and hybrid mining (play a stretch by hand, set the sampling bubble) works with it, as it does with
  the browser miner. Switching the visualisations off leaves only the search running.
- **Windows portable zip**: `SM64Coin-@VERSION@-windows-x86_64-portable.zip` next to the installer. Unzip anywhere and
  run `SM64Coin.exe`; the chain, wallets, ROM copy and settings stay in the `data` folder beside it. Needs the
  Microsoft Edge WebView2 runtime (built into Windows 11 and current Windows 10; the installer would fetch it).
- Fixes: the 3D cloud no longer goes black after a hand-played session (a leaked WebGL context; the view also
  recovers if the browser drops its context); a wallet created with more KDF rounds opens again after an app
  restart; the miner's visualisation channel checks every frame and request at the app boundary.

### 0.5.1: the SM64Coin desktop app (preview)

Same chain as 0.5.0: no reset, upgrade in place (your blocks and wallet carry over).

- **New: the SM64Coin desktop app** (`SM64Coin_*`): the website's pages in a native window, with a local
  full node, the native auto-miner (several times faster than the browser) and a wallet on Monero's
  `wallet-rpc` (password to open, auto-lock after 10 idle minutes, password before sending and before
  showing your seed). Linux: `.deb` or `.AppImage` (portable: `chmod +x` and run); Windows: `*-setup.exe`;
  macOS: `.dmg`. "Use my website passkey" brings a passkey wallet over from the website through your
  browser; or restore with your seed words.
  **Preview:** the installers are not code-signed yet (Windows: "More info, Run anyway"; macOS: right-click,
  Open), and only the Linux build has been tested by hand. The classic wallet app stays in the bundles below.
- **Public nodes behind a proxy or tunnel no longer go dark**: the node never bans loopback RPC clients
  (a few stale browser submissions had banned every visitor of eboy.au).
- Website: Enter works again in password and name boxes; a footer says which code is Monero's
  ([what changed](https://github.com/Gumbo64/sm64coin/blob/main/docs/MONERO-FORK.md)).

### 0.5.0: a new chain

**0.5 starts a new chain** (new network ids and genesis block; heights restart at 0). 0.4 coins and blocks do not
carry over, and 0.4 and 0.5 nodes refuse each other. The network starts as soon as nodes run 0.5.

**What you have to do**
- **Node operators:** install 0.5.0 and start with a **fresh data dir** (move `~/.sm64coin` aside; keep your wallet
  files). Ports, address prefixes and the ROM are unchanged.
- **Wallets:** keep your `.keys` file and let the wallet rescan from height 0. The website's wallet does this on its own.
- **Miners:** update `automine` (or the wallet app, or use the website).

**What changes**
- **Cap switches are blocks:** pressing the red (wing), green (metal) or blue (vanish) cap switch is progress, like a
  star or a Bowser key: each colour once per cycle (a cycle is now about 76 blocks). A restart's delay still counts
  only stars, keys and the Grand Star.
- **Mined coins unlock after 6 blocks** (about 2 hours) instead of 72 (24 hours).
- **Miners:** smaller proofs from the exact input canonicaliser (about 80x faster than the replay-guided one; on by
  default in the web miner), and a found run is handed over within about 10 s instead of at the end of the search step.


### 0.4.1: a new chain

**0.4 starts a new chain** (new network ids and genesis block; heights restart at 0). 0.3 coins and blocks do
not carry over, and 0.3 and 0.4 nodes refuse each other. There is no waiting period: the network starts as soon
as nodes run 0.4.

**What you have to do**
- **Node operators:** install 0.4.1 and start with a **fresh data dir** (move `~/.sm64coin` aside; keep your
  wallet files). Ports, address prefixes and the ROM are unchanged.
- **Wallets:** keep your `.keys` file, delete the wallet cache and let the wallet rescan from height 0 (a 0.3 cache
  is refused: "Genesis block mismatch"). The website's wallet rescans on its own.
- **Miners:** update `automine` (or the wallet app, or use the website). Nothing to configure: a run that can win
  is submitted as a block, a run that loses its height is published as a proof and paid.
- **Bot authors:** proofs now start with a committed header (`frames, start, input_hash`) before the encoding;
  the SDKs (Python `sm64-binds>=2.3.4`, the web client `sm64-binds-frontend ^2.3.4`) do this. BOTS.md has the layout.

**What changes**
- **Committed inputs:** a proof's header commits the hash of its inputs and the block id uses that hash, so proof
  encodings are transport. Nodes negotiate the codecs they decode (0-2 always; state codecs 3-7, 6 and 7 new and
  smaller) and re-encode for each peer. A badly encoded copy never makes a block invalid.
- **Included proofs:** each block carries up to 64 near-best runs for the previous height (other miners' runs
  within 60 s of auction time of the winner, one per key): the chain archives them.
- **Proof rewards:** each included proof is paid min(R/128, 25 % of R / n) of new issuance, to the payout address
  (or a fresh subaddress) its template committed; the block's miner keeps its full reward.
- **Fork choice:** a chain must be longer AND heavier (blocks plus counted proofs) to win; the 36-block reorg cap
  and the equivocation penalty stay (an equivocated block weighs nothing).
- **Game build:** sm64-binds 2.1.4 (game build `5e3b86e1`): splash spawns, the object memory pools (e.g. the
  Wiggler) and the surface pools (2,300 surfaces / 7,000 nodes, counted per area from 0) behave as on the N64.
  0.4.1 replaces the unlaunched 0.4.0, whose game build (2.1.2) still had the port's unbounded surface pools.
- **Launch floor:** blocks stamped before the release are refused (a chain backdated from the genesis block could
  otherwise be pre-mined); the floor is already past, so nobody waits.
- **Hardening:** a header byte the block id doesn't cover can't make a block invalid; a bad copy of a block is
  forgotten and another peer's copy tried; the proof pool budgets replays per peer; the sync pre-pass only
  replays proofs of verified blocks.
- **Miners and SDKs:** wait for the height's proofs before building on a block (`sm64_records_ready_at`), a fresh
  payout subaddress per template (`--payout-view-secret`), the input normaliser (smaller proofs) and every 0.3.1-0.3.3
  fix (the RLE decode overflow fix, "save and quit" starts, refusal reasons, `sm64coin_version` in `get_info`).
- **Wallets, explorers:** block ids come from the proof header (no decoder); `get_block` returns
  `sm64_input_hash`, `sm64_codec` and `sm64_records_bytes`. Wallet binaries and the web wallet engine are rebuilt
  (new block field).
- **Website:** everything from 0.3.3 (block pages with the envelope graph and replay picker, versions on the Info
  page, the chain check, now against the 0.4 genesis).

Update: required

<!--
  Update notices: the wallet app, the website, sm64coin-node and sm64coin-automine look at the latest
  release (GitHub API, cached 6 h) and tell users when theirs is older. To make the notice a stronger
  warning ("update required"), add a line consisting of only the words  Update: required  (case does
  not matter) to the release body when you publish. Only a whole line counts, so this comment, which
  is published with the notes, does not trigger it.
  Opt out: SM64COIN_NO_UPDATE_CHECK=1 (programs), a setting in the wallet app.
-->
