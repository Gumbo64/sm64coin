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
