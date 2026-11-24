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


### Changes

- **Mine in your browser:** new Auto-mine page runs the miner in WebAssembly, one worker per thread; workers share the castle step and sync their search trees. It keeps mining across pages (status corner, top right).
- **Auto-mine visualisations:** live game view, search point cloud, envelope graph and popups; switch them off for full speed. Frames/s shown on the site and in the wallet app.
- **Lite web wallet:** create, restore (seed or keys), receive and send in the browser; keys never leave it. Backups, desktop `.keys` import, optional passkey wallet.
- **Update notices** in the wallet app, website and `sm64coin-node`.
- **Cycle reset:** all miners restart from power-on when the chain allows it and the parent is stuck.
- **Faster mining starts:** miners load the tip's state from the node (`get_sm64_state`, nodes need 0.2.8).
- **Wallet app serves the website and leaderboard locally**; GitHub Pages only as a fallback.
- **Servers:** `sm64coin-node` (+ systemd unit) replaces the `sm64coin` launcher.
- `sm64coin-automine` and `sm64coin-leaderboard` are in the bundles.

Consensus is unchanged.

<!--
  Update notices: the wallet app, the website, sm64coin-node and sm64coin-automine look at the latest
  release (GitHub API, cached 6 h) and tell users when theirs is older. To make the notice a stronger
  warning ("update required"), add a line consisting of only the words  Update: required  (case does
  not matter) to the release body when you publish. Only a whole line counts, so this comment, which
  is published with the notes, does not trigger it.
  Opt out: SM64COIN_NO_UPDATE_CHECK=1 (programs), a setting in the wallet app.
-->
