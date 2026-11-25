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


### A new chain

**0.3 starts a new chain on 2026-11-26 at 00:00 UTC (10:00 AEST). The 0.2 chain ends there.** 0.3 nodes only talk to 0.3 nodes.

- **Node operators:** install 0.3 and start with a fresh data dir (move or delete `~/.sm64coin`, keep your wallet files). Ports are unchanged.
- **Wallets:** the same keys and addresses work. The wallet app and the web wallet rescan the new chain on their own; with the CLI wallet, keep the `.keys` file, delete the cache file and restore from height 0.
- **Miners and bots:** update. Blocks without a signed proof are invalid on 0.3 (see `BOTS.md`, "Signed proofs").

### Changes

- **Signed proofs:** every block's run is signed by a key the miner commits in its template, so nobody else can submit a variant of your run. Checked in about 0.1 ms, before any replay.
- **One run per block:** a miner signs one run per template. Two different signed runs from one key are equivocation, and nodes build around the equivocator's block. Miners keep improving privately and submit their best run just before it is due.
- **20-minute blocks** (were 5). One-thread and browser miners can now win blocks; rewards per day are unchanged.
- **Fairer races:** within 30 s of the first valid block, a strictly shorter run from another miner takes its place (equal lengths: the same winner at every node), and nodes no longer relay stale-stamped blocks.
- **Replay queue:** nodes check signatures on arrival, replay claims only when they are due and only if they could matter, shortest first, in parallel, outside the blockchain lock.
- **Finality and locks:** nodes never reorganise more than 36 blocks (about 12 h; operators can override). Mined rewards unlock after 72 blocks (24 h); received coins are spendable after 3 blocks (1 h).
- **Leaderboard:** joining sends the heights of your mined blocks, which nodes check instead of scanning the chain; the board updates live with each block. Operators can turn it off (`--sm64-no-leaderboard`).
- **Hardening:** a security and DoS review of the node, RPC, leaderboard, wallet app and launcher (see `docs/DOS.md`). The wallet app's and launcher's local site only pass the website's own calls to the node.
- The website's updates since 0.2.9 (save states, full-screen toolbars, touch goals, automatic worker sync), now in the wallet app's local site too.

Update: required

<!--
  Update notices: the wallet app, the website, sm64coin-node and sm64coin-automine look at the latest
  release (GitHub API, cached 6 h) and tell users when theirs is older. To make the notice a stronger
  warning ("update required"), add a line consisting of only the words  Update: required  (case does
  not matter) to the release body when you publish. Only a whole line counts, so this comment, which
  is published with the notes, does not trigger it.
  Opt out: SM64COIN_NO_UPDATE_CHECK=1 (programs), a setting in the wallet app.
-->
