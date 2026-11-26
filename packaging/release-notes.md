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


### 0.4.0: a new chain

**0.4 starts a new chain** (new network ids and genesis block; heights restart at 0). 0.3 coins and blocks do
not carry over, and 0.3 and 0.4 nodes refuse each other. There is no waiting period: the network starts as soon
as nodes run 0.4.

**What you have to do**
- **Node operators:** install 0.4.0 and start with a **fresh data dir** (move `~/.sm64coin` aside; keep your
  wallet files). Ports, address prefixes and the ROM are unchanged.
- **Wallets:** keep your `.keys` file, delete the wallet cache and let the wallet rescan from height 0 (a 0.3 cache
  is refused: "Genesis block mismatch"). The website's wallet rescans on its own.
- **Miners:** update `automine` (or the wallet app, or use the website). Nothing to configure: a run that can win
  is submitted as a block, a run that loses its height is published as a proof and paid.
- **Bot authors:** proofs now start with a committed header (`frames, start, input_hash`) before the encoding;
  the SDKs (Python `sm64-binds>=2.3.3`, the web client `sm64-binds-frontend ^2.3.3`) do this. BOTS.md has the layout.

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
- **Game build:** sm64-binds 2.1.2 (splash spawns and the object memory pools behave as on the N64, e.g. the
  Wiggler).
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
