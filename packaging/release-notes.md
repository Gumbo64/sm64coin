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

- **Browser auto-miner**: the website has an Auto-mine page (`/automine`) that runs the miner's tree-search
  campaign inside the browser (WebAssembly, several workers), against your node or a public one.
- **Reset-cycle policy for all miners**: `never` / `when-stuck` / `always-when-allowed`, shared by
  `sm64coin-automine`, the web auto-miner and the wallet's Mine page (new reset-cycle controls).
- **Start from the node's state**: miners fetch the chain tip's end state with the new `get_sm64_state`
  RPC (gzip) instead of replaying the whole cycle; they fall back to replaying when the node is older.
  Nodes must run 0.2.8 for this.
- **Wallet app serves the website locally**: the Wallet and Leaderboard/Mine pages open the bundled `web/`
  folder (kept next to the wallet app) from a local server, with the leaderboard server started alongside
  it; GitHub Pages is only the last resort. First run starts a local node in `~/.sm64coin/node`, asks for the ROM
  and shows readable start errors; the miner runs at low CPU priority.
- **Peers and seeds**: the seed nodes are priority peers of the wallet's local node, as they are for `sm64coin-node`.
- **The `sm64coin` launcher is replaced** by the `sm64coin-node` script (and a systemd unit) for servers.
- `sm64coin-automine` (and its `rrtrun` child) and `sm64coin-leaderboard` are in the bundles.
- Fixes: the Explorer's block sizes and one `get_sm64_cycle` per tip per visit; end states are written to disk
  only for newly verified blocks.

Consensus is unchanged (same game build, same block rules as 0.2.7).
