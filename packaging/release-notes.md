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
   Mining page: **Start** runs the automated miner, **Play to mine** opens the website.

**Servers** (no wallet app): `./sm64coin-node` runs `sm64coind` with the seeds and the ROM; `sm64coin-node.service`
is a systemd unit. Open TCP 16480. Details in `README.txt`.

Nothing else needs to be installed. Verify a download with `sha256sum -c SHA256SUMS --ignore-missing`.
The `sm64coin` launcher is no longer shipped.

Not in this release: Windows and macOS builds, and the automated miner (`sm64coin-automine`).

### Changes

(edit before publishing)
