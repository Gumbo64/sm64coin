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


### 0.3.1: a point release of the 0.3 chain (no consensus change)

Every block that is valid on 0.3.0 is valid on 0.3.1 and every invalid block stays invalid (the consensus
tests and the 234 proof-id vectors pass unchanged). The chain, the genesis block, the ports and the data
dir are the same: **just replace the binaries; no resync.**

- **Smaller proofs (miners).** The miners (`automine`, the Rust SDK, the web miner on request) run an
  input normaliser before sealing: pads that do not change what the game does (the stick in the air and in
  cutscenes, buttons that nothing reads) are replaced by cheaper ones, then the proof is coded with the
  existing codecs. Every candidate is replayed first; if the end state, the perturbation state, the frame
  count or the success frame differ at all, the run as played is sealed. Seconds per block, on the
  encoder only; `automine --no-normalise` skips it. On the 34 v0.2 mainnet blocks (143,505 frames) the proofs shrink from 60,639 to 47,651 bytes (-21.4 %); a fresh testnet block went 1,814 to 1,577 bytes (-13 %). Costs about 2-3 ms per frame (10-60 s for a 3,000-9,000 frame run).
- **No reserved controller bits.** Mupen64Plus treats a pad with both reserved bits (0x00C0) as a console
  reset. The game never reads them (tests/reserved_bits.rs) and the poke pads never use them; miners now
  clear them before sealing. Consensus still accepts them. The chain export tool gets a
  `--clear-reserved-bits` flag for `.m64` output.
- **Fix (tests/nodes):** a state the node verified long ago is no longer evicted from its cache while it
  is the tip's parent (this was a flaky `NeedParent` in the test suite).
- **Tools:** `pow/tools/board/` has the leaderboard's functional checks and benches, runnable against a
  throwaway build.

Update: recommended

<!--
  Update notices: the wallet app, the website, sm64coin-node and sm64coin-automine look at the latest
  release (GitHub API, cached 6 h) and tell users when theirs is older. To make the notice a stronger
  warning ("update required"), add a line consisting of only the words  Update: required  (case does
  not matter) to the release body when you publish. Only a whole line counts, so this comment, which
  is published with the notes, does not trigger it.
  Opt out: SM64COIN_NO_UPDATE_CHECK=1 (programs), a setting in the wallet app.
-->
