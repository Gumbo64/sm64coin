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


### 0.3.3: fixes (no consensus change)

- **Miners:** `automine` no longer exits when a run's pokes pick "save and quit" in the star's save menu (about 1.5 % of
  templates): the start settles through the title screen, and any failed attempt moves on to the next template.
- **Nodes:** `get_info` and `get_version` report `sm64coin_version`, `sm64_game_build` and `sm64_rules`, also on the
  restricted RPC, so wallets and the website can show which node version they talk to.
- **Website** (also live on GitHub Pages): versions on the Info page with a short explainer of the auction; block pages
  with the envelope graph and a replay picker; faster replays from saved block states; the auto-miner's position goal
  as its own objective with a strength setting; new areas shared between workers within a second; faster start-up;
  a check that the node is on this chain (old 0.2 nodes fall back to the public node).

### 0.3.2: a security fix (no consensus change)

- **Fix:** a crafted run-length proof with a run length near 2^64 could wrap the decoder's limit check and
  crash a node (or, on 32-bit/WebAssembly builds, be read as a different length). The length is now checked
  before any addition or cast, in the node and in the web wallet. Every valid block is decoded exactly as
  before. Found by the new formal checks (Kani). **Update your node.**

**0.3.1** (still in this release): smaller proofs from the miners' input normaliser, no reserved controller
bits in miners' runs, clearer RPC refusals, a cache fix, and the leaderboard's test tools.
Same chain, genesis, ports and data dir as 0.3.0: just replace the binaries.

**Coming from 0.2?** 0.3 started a new chain on 2026-11-26 00:00 UTC: use a fresh data dir and let your wallet
rescan (see the [0.3.0 notes](https://github.com/Gumbo64/sm64coin/releases/tag/v0.3.0)).

Update: required

<!--
  Update notices: the wallet app, the website, sm64coin-node and sm64coin-automine look at the latest
  release (GitHub API, cached 6 h) and tell users when theirs is older. To make the notice a stronger
  warning ("update required"), add a line consisting of only the words  Update: required  (case does
  not matter) to the release body when you publish. Only a whole line counts, so this comment, which
  is published with the notes, does not trigger it.
  Opt out: SM64COIN_NO_UPDATE_CHECK=1 (programs), a setting in the wallet app.
-->
