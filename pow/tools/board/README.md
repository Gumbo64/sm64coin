# Leaderboard checks (functional and bench)

Scripts that drive a **throwaway** `sm64coind` over RPC and check the leaderboard
(`sm64_board`, `sm64_board_join`, `sm64_board_set`, `--sm64-no-leaderboard`; the Rust side is
`pow/leaderboard`). They run on a regtest chain, so they need a test build of the daemon:

1. **Test-only patch.** `testonly.patch` (apply it to the `monero/` submodule of a scratch checkout,
   never to a release build) lets a regtest chain take blocks with arbitrary timestamps and skips the
   proof replay on FAKECHAIN, so `generateblocks` can mint hundreds of blocks in seconds. Every change
   is marked `TESTONLY`; the timestamp rules relax only when the environment has `SM64_TESTONLY_TS`
   (the scripts set it), the replay skip is `FAKECHAIN` only.

   ```
   cd monero && git apply ../pow/tools/board/testonly.patch
   mkdir -p build/test && cd build/test
   cmake -D CMAKE_BUILD_TYPE=Release -D BUILD_TESTS=OFF -D USE_DEVICE_TREZOR=OFF -D MANUAL_SUBMODULES=1 ../.. && make -j4 daemon
   ```

2. **Wallets.** `gen.py N OUT.json` makes N board identities (address, view key, a signed name and five
   signed renames each) through a running `monero-wallet-rpc` on 127.0.0.1:28483 (wallet open, no
   password; it creates `w1..w{N-1}`). Both scripts use wallet 149 as the filler miner: make 150.

   ```
   python3 gen.py 150 wallets.json
   ```

3. **Run** (the ROM from `SM64COIN_ROM`, default `../../../baserom.us.z64`; the wallets from
   `BOARD_WALLETS`, default `./wallets.json`; data dirs under `BOARD_TMP`, default `/tmp`):

   ```
   python3 func.py /path/to/test/bin/monerod           # prints PASS/FAIL lines, ALL PASS at the end
   python3 bench.py /path/to/test/bin/monerod base 100  # join / rename floods and reorg flips: CPU and wall times
   python3 bench.py /path/to/test/bin/monerod after 100 # the same on a second build (ports differ), to compare
   ```

`func.py` checks: an honest join is queued and counted; lying about heights is refused; no heights
means "update your wallet"; duplicate and out-of-range heights are invalid; a rename is accepted and a
second one within 10 minutes is refused; a live block is counted without a rescan; a reorg forgets the
hit; the off switch refuses joins and reports `disabled`; blocks mined while off are caught up; the
board survives a restart without re-verification; `--sm64-no-leaderboard` starts disabled.

`bench.py` measures idle CPU, honest joins (40 wallets), a flood of valid-but-never-mined wallets
claiming random heights, a rename flood, and three reorg flips; it writes `result.json` in its data
dir. `bench-*.log` in the repo history of the board work (`monero-board-tests/`) are the numbers
before and after the board's rescan fixes: idle CPU 0.01 s per 20 s, 40 honest joins 0.07 s to reply.
