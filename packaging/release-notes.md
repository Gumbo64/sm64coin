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

- **One Mine page:** play, auto-mine, or both at once (hybrid mining): play from any node of the search, shift + left click sets a position goal for the workers, your played paths join the search. `/automine` opens it.
- **Run sharing:** once a worker finds a run, every worker searches the same block for a shorter one; the shortest replaces the held run and the block is submitted once.
- **Highlight reel:** new places and records play as they happen; any node of the cloud plays instantly.
- **Places reached chart:** how many floors the workers have reached on their course, and whether they are exploring, refining or stuck.
- **Phones:** touch controls, full-screen play and a layout that fits.
- **Nodes:** hardened against block-replay spam (relay policy; consensus unchanged). Update your node.
- **Fixes** from a review of 0.2.8: one worker per slot, a held run gives up when a new block lands, the reel and game view on reloads, the wallet proof-id checks in CI, and more.

Consensus is unchanged.

<!--
  Update notices: the wallet app, the website, sm64coin-node and sm64coin-automine look at the latest
  release (GitHub API, cached 6 h) and tell users when theirs is older. To make the notice a stronger
  warning ("update required"), add a line consisting of only the words  Update: required  (case does
  not matter) to the release body when you publish. Only a whole line counts, so this comment, which
  is published with the notes, does not trigger it.
  Opt out: SM64COIN_NO_UPDATE_CHECK=1 (programs), a setting in the wallet app.
-->
