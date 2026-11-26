# Reading the SM64Coin core

The core is what SM64Coin adds to Monero that decides whether a block is
valid: the Mario 64 proof (format and codecs), its verification by replaying
the run on an emulated N64, the controller perturbation ("pokes"), the rules
(time limit, retarget, restarts, fork choice), the hold policy, the C ABI
between the daemon and Rust, and the `SM64COIN`-marked changes inside the
Monero fork. Everything else is stock Monero, user interface, or tooling (see
[Safe to skip](#safe-to-skip)).

It is about 4,000 lines of Rust in three crates, plus about 1,000 lines of
C++ glue in the fork:

| crate | path | what |
|---|---|---|
| `sm64pow` | `pow/sm64pow` | consensus: rules, proofs, replay, chain, C ABI (`libsm64pow.a`, linked into the daemon) |
| `sm64coin-perturb` | `pow/perturb` | the pokes, and what is read from the game's memory. No dependencies, so the browser runs the same code |
| `sm64coin-statecodec` | `pow/statecodec` | the state-aware proof codecs 3-5: a range coder plus integer context-mixing models (pure, also used by the browser) |
| Monero fork | `monero/src/sm64coin/`, `SM64COIN` markers | the daemon's hooks: each call site is a one-liner marked `SM64COIN` |

## A block, end to end

**Mining.**

1. The miner calls `get_block_template`. Besides the usual header the template
   carries the **seed**, `sm64_seed_hash`: `cn_fast_hash` of the block hashing
   blob with the timestamp set to zero (`Blockchain::sm64_seed`). It also
   carries the task (`sm64_max_frames` now, the poke settings), where the run
   may start (`sm64_start`: `parent`, `power-on` or `either`, plus
   `sm64_power_on_at` and `sm64_restart_delay` for a restart), and, if the
   request gave `sm64_frames: N`, the second a run of N frames becomes valid
   (`sm64_valid_at`).
2. The miner loads the parent's end state (it replays the current cycle, or
   fetches the state with `get_sm64_state`) and plays from there. Every frame
   goes through the perturbation (`game::perturbed_step`). The `Perturber`
   (seeded from all 32 bytes of the seed) either passes the wanted pad through
   or replaces it with a random one (a **poke**). After the frame it mixes a
   digest of the game state into its generators. Pokes run on the game's own
   clock, so pausing neither spends nor rerolls them.
3. The run ends on the first frame where the game's progress (stars + Bowser
   keys + Grand Star) goes up by one. The miner **seals** it
   (`mining::seal_proof_at`): it replays the played pads, drops the poked
   frames (they are regenerated from the seed, never stored), encodes the rest
   with every codec and keeps the smallest. The result is
   `varint(frames) || start || codec id || body`.
4. The miner puts the blob in `block::sm64_proof` (a new field of the
   block blob), signs it (v0.3: `sm64_sig`, the last field, with the
   throwaway key it put in the coinbase extra nonce when it asked for the
   template; [V0.3-NEW-CHAIN.md](V0.3-NEW-CHAIN.md), `sign.rs`), stamps the
   block "now", and calls `submit_block`. The daemon
   re-encodes a stateless proof (codecs 0-2) to the smallest of them. That
   doesn't change the block id.

**Verifying** (every node, for every block):

5. **Block id** = `cn_fast_hash(hashing blob || proof_id(sm64_proof) || sm64_sig)`
   (`get_block_id_blob`). Before any replay, `sm64_sig` must verify with the
   miner key the coinbase commits (`chain::verify_signed_block`). `proof_id` hashes the decoded run for codecs 0-2 and
   the canonical bytes for codecs 3-5.
6. **Timestamp**: strictly after the parent (v0.4: no launch-time floor; `Rules::launch_timestamp` is 0),
   and at most 120 s ahead of the node's clock
   (`Blockchain::sm64_check_timestamp`, `CRYPTONOTE_BLOCK_FUTURE_TIME_LIMIT`).
7. **Hold** (relay policy, not consensus): a block extending the tip that is
   valid at its own timestamp but too long for the limit by the node's clock
   is held, not rejected, until the first second it fits
   (`Blockchain::sm64_hold` -> `hold::hold_until`). So stamping ahead of the
   clock gains nothing.
8. **Proof**: `Blockchain::sm64_check_proof` -> `sm64coin::verify_block` ->
   `sm64pow_verify_block` -> `chain::verify_block`. The limit is
   `rules().max_frames(difficulty, seconds since the parent)`. The start rules
   decide which states the run may begin from (`replay::start_game`). The
   replay (`replay::replay` / `replay_state`) then requires the run to be within
   the limit, every non-poked frame to have an input, the success condition to
   hold first on exactly the last frame, and no inputs left over. A valid run's
   end state is cached for the next block.
9. **Missing parent state** (`-2`): the daemon walks back to a block it can
   replay from and replays forward with difficulty 0, meaning no time limit
   since those blocks are already accepted (`Blockchain::sm64_rebuild`). Then it
   verifies again. A whole cycle replays in seconds.
10. **Fork choice and retarget**: the chain with more blocks wins, and a tie
    keeps the one seen first (`Rules::weigh_by_blocks`). Difficulty is
    retargeted the Monero way over 60 blocks toward 1200 s
    (`Rules::next_difficulty`). Difficulty only sets the time limit; the pokes
    are the same at every difficulty.

**Syncing**: a batch of blocks is first replayed in parallel, one chain of
blocks at a time (`preverify`). The sequential check then only re-checks the
rules that depend on the timestamp and the parent.

## Reading order

Line counts are approximate. Paths are under `pow/` unless they start with
`monero/`. Every Rust file opens with a module comment that names the next
file to read.

| # | file | lines | what it is | look for |
|---|---|---|---|---|
| 1 | `sm64pow/src/lib.rs` | 70 | the map: layers, module list, re-exports | the layer list in the crate doc |
| 2 | `sm64pow/src/rules.rs` | 460 | **all consensus parameters** as pure functions | `STAR_CYCLE` (every constant), `StarCycle::max_frames` (the auction: the limit grows every second after the parent), `restart_delay`, `is_success`/`ends_cycle`, `monero_retarget`, and the tests at the bottom |
| 3 | `sm64pow/src/hold.rs` | 70 | the hold policy | `hold_until`: a binary search for the first second the run fits |
| 4 | `sm64pow/src/seed.rs` | 80 | the seed | why the timestamp is excluded |
| 4b | `sm64pow/src/sign.rs` | 230 | signed proofs (v0.3): the miner key in the coinbase extra nonce, Monero's Schnorr signature | `message`, `check`, `Keypair::sign` (deterministic) |
| 5 | `sm64pow/src/proof.rs` | 330 | the proof blob, codecs 0 raw / 1 run-length / 2 xz | `split_blob`, `decode_proof`, the canonical varint, the zip-bomb caps |
| 6 | `perturb/src/lib.rs` | 420 | **the pokes** | `Perturber::new` (two xoshiro256** streams from the 32-byte seed), `pad` (only on a tick of the game clock), `is_poke` (5 bursts of 2 per 100 ticks), `settle` (the refund when the frame didn't simulate; the per-frame digest mix) |
| 7 | `perturb/src/sm64.rs` | 330 | what is read from Mario 64's memory | `regions` and `view` (the clock and the digest), `Progress` and `points`, `Features` (the codecs' inputs) |
| 8 | `sm64pow/src/emulator.rs` | 70 | the ROM and the pinned game build | `GAME_BUILD`, `init` refusing any other build |
| 9 | `sm64pow/src/game.rs` | 220 | a game bundled with its perturbation | `perturbed_step` (the one way a frame is played), `mining_start`, `read_progress` |
| 10 | `sm64pow/src/statecodec.rs` | 150 | the state-aware codecs 3-5, fed by the replay | `encode_proof_state`, `decode_proof_state`, `state_proof_id`. The models themselves are `statecodec/src/` (`range.rs` first, then `statectx.rs`, `statemix.rs`); skim them, they are compression |
| 11 | `sm64pow/src/proof_id.rs` | 70 | what a block id commits to | the three domain tags `sm64run2` / `sm64state` / `sm64bad!` |
| 12 | `sm64pow/src/replay.rs` | 210 | **verifying one run** | `start_game` (genesis child and post-Grand-Star blocks start at power-on; a restart pays a delay), `replay`, and `replay_state` (the canonical-encoding check) |
| 13 | `sm64pow/src/cache.rs` | 370 | end states between blocks (memory and disk) | `run_key`: restamped copies of a block share one entry (red team finding F1). The disk format is at the bottom |
| 14 | `sm64pow/src/preverify.rs` | 145 | the parallel sync pre-pass | it never decides validity |
| 14b | `sm64pow/src/admit.rs` | 590 | which blocks earn a replay at all (relay policy, not consensus; [DOS.md](DOS.md)) | `admit_alt`, `admit_relay`, `RpcScope` |
| 14c | `sm64pow/src/equivocation.rs` | 260 | two signed runs for one template (relay policy, v0.3) | `judge_sibling`, `Evidence`, `choose_parent` |
| 15 | `sm64pow/src/chain.rs` | 200 | **the verdict per block** | `check`: the known-run fast path (re-checks only the timestamp rules), the limit, the restart delay, `NeedParent` |
| 16 | `sm64pow/src/ffi.rs`, `sm64pow/sm64pow.h` | 525 + 70 | the C ABI | sections: setup, verifying blocks, proof blobs, rules. Every function catches panics |
| 17 | `monero/src/sm64coin/sm64coin.{h,cpp}` | 105 + 225 | the daemon's whole interface to proof-of-play | type adaptation only |
| 18 | `monero/src/cryptonote_core/blockchain.cpp:5836-6117` | 290 | the daemon's hooks | `sm64_check_timestamp`, `sm64_seed`, `sm64_rebuild`, `sm64_check_proof`, `sm64_hold` |

After that, if you want the miner's side: `sm64pow/src/mining.rs` (sealing),
then `pow/miner` (the search; see below).

### Layers

Each module only uses modules in lower layers. The script that checked this
(module edges from `crate::` paths, Tarjan SCC) found the old layout had a
cycle `lib <-> chain <-> game <-> statecodec`. There is none now.

```
 6  ffi ............................................ C ABI for the daemon (sm64pow.h)
 5  chain ── preverify ── cache ── admit ── equivocation  a block's verdict, sync pre-pass, end states, relay policy
 4  replay ......................................... one run: start rules, replay, success on the last frame
 3  proof_id ── statecodec ......................... block-id commitment; codecs 3-5 (replay-fed)
 2  game ── emulator ............................... game + perturbation; ROM / game build
 1  rules  hold  seed  sign  proof ................. pure: parameters, hold policy, seed, signatures, blob + codecs 0-2
 0  sm64coin-perturb   sm64coin-statecodec ......... pokes + memory map; pure codec models

 beside the consensus path: mining (sealing, standalone verify), statecodec's research tooling
```

## SM64COIN changes in the Monero fork

The fork is `monero/` (branch `sm64coin`). Line numbers are at the pinned
submodule commit `c4ca7efe0`. `grep -rn SM64COIN monero/src` lists all 157
markers. Here they are grouped by what they do.

**Proof-of-play: the consensus hooks**

- `src/sm64coin/sm64coin.h`, `sm64coin.cpp`, `CMakeLists.txt`: the wrapper around `sm64pow.h`; CMake builds `pow/sm64pow` with cargo and links `libsm64pow.a`.
- `src/CMakeLists.txt:93`, `src/cryptonote_basic/CMakeLists.txt:69`: add and link the `sm64coin` library.
- `src/cryptonote_basic/cryptonote_basic.h:594-612`: `block::sm64_proof`, the new last field of the block blob (at most `SM64COIN_MAX_PROOF_BYTES` = 256 KiB).
- `src/cryptonote_basic/cryptonote_format_utils.cpp:1479-1500`: `get_block_id_blob`: the block id also commits to `proof_id(sm64_proof)`.
- `src/cryptonote_core/blockchain.cpp:5836-6117`: the hooks: `sm64_parent_timestamp`, `sm64_check_timestamp` (after the parent), `sm64_effective_difficulty`, `sm64_seed` (timestamp zeroed), `sm64_parent_ref`, `sm64_rebuild`, `sm64_check_proof`, `sm64_played`, `sm64_cycle`, `sm64_progress`, `sm64_hold`, `sm64_take_due`, `sm64_held_until`.
- `blockchain.cpp:4113, 4131, 4183` (`handle_block_to_main_chain`): timestamp check, retarget difficulty, `sm64_check_proof` in place of `check_hash`.
- `blockchain.cpp:1931, 1949, 1976` (`handle_alternative_block`): the same three for alternative chains.
- `blockchain.cpp:2110`: fork choice by block count (`weigh_by_blocks`) instead of cumulative difficulty.
- `blockchain.cpp:4715` (`add_new_block`): `sm64_hold`, so a block is held until its run fits by this node's clock.
- Denial of service ([DOS.md](DOS.md)): `sm64_skip_alt` in `handle_alternative_block`, the admission checks in `sm64_hold`, `sm64coin::source_scope` in `cryptonote_protocol_handler.inl` (relayed blocks, sync spans) and `on_submitblock`, `sm64coin::rpc_scope` in the RPC calls that replay.
- `blockchain.cpp:5199` (`prepare_handle_incoming_blocks`): the parallel pre-pass of a sync batch.
- `blockchain.cpp:1625` (`create_block_template`): a template is stamped after its parent.
- `blockchain.cpp:954` (`recalculate_difficulties`): disabled with `if (true || ...)`.
- `blockchain.cpp:1321` (`prevalidate_miner_transaction`): the coinbase unlock window (72 blocks; only the genesis coinbase carries 10).
- `blockchain.cpp:5656` (`load_compiled_in_block_hashes`): Monero's compiled-in hashes are disabled.
- `blockchain.h:55, 341-355, 1259-1261`: declarations of the hooks and the held-block map.
- `src/cryptonote_core/cryptonote_core.cpp:96-122, 339, 349`: `--sm64-rom` and `--sm64-solo`.
- `cryptonote_core.cpp:512`: loads the ROM at start-up.
- `cryptonote_core.cpp:710`: `set_state_dir(<data>/sm64_states)`.
- `cryptonote_core.cpp:1610, 1678` (`sm64_release_held`, `on_idle`): processes held blocks once they are due.
- `cryptonote_core.h:73, 116`: declarations.
- `src/cryptonote_basic/difficulty.cpp:41, 205`: `next_difficulty` delegates to `Rules::next_difficulty`.
- `src/cryptonote_core/cryptonote_tx_utils.cpp:711`: there is no hash-based proof of work; the "long hash" is a fast hash kept for RPC fields.
- `src/cryptonote_basic/miner.cpp:372`: the built-in hash miner is disabled.
- `src/serialization/json_archive.h:196`, `json_object.cpp:30, 322, 342`: `sm64_proof` as hex in JSON.

**Chain parameters** (`src/cryptonote_config.h`)

- `:44` coinbase unlock window 72 (Monero: 60; v0.2: 4).
- `:48` future time limit 120 s (was 2 h).
- `:49` `SM64COIN_MAX_PROOF_BYTES`.
- `:81-85` target 1200 s, window 60, lag 0, cut 6. These must agree with `rules.rs`.
- `:166` name `sm64coin`.
- `:228-300` address prefixes (`SM64`, `SM6i`, `SM6s`), ports 16480/26480/36480, network ids, genesis tx and nonces (the genesis coinbase is burned to the RingCT H point).
- `src/hardforks/hardforks.cpp:35-51`: Monero's latest rules from block 1.
- `src/checkpoints/checkpoints.cpp:198, 318`: no Monero checkpoints, no DNS checkpoints.
- `src/p2p/net_node.inl:722, 757`: no seed nodes.
- `src/version.cpp.in:3`: release name.

**RPC for miners and explorers** (not consensus)

- `src/rpc/core_rpc_server_commands_defs.h:744-755, 983-1082, 1088-1174, 1569-1605`: the `sm64_*` fields of `get_info`, `get_block_template` (`sm64_start`, `sm64_power_on_at`, `sm64_restart_delay`, `sm64_valid_at`, ...) and `get_block` (`sm64_pads`, `sm64_played_pads`, `sm64_event`, `sm64_seed_hash`, ...), plus the `get_sm64_cycle`, `get_sm64_state` and `sm64_board*` commands.
- `src/rpc/core_rpc_server.cpp:527` (`on_get_info`), `:2015-2077` (`on_getblocktemplate`), `:2345` (`on_submitblock`: re-encode to the smallest stateless codec), `:2853-2877` (`on_get_block`), `:2890-2960` (board, `get_sm64_state`, `get_sm64_cycle`).
- `src/rpc/core_rpc_server.h:165-168, 247-250`: registration of those commands.

**Mining leaderboard** (not consensus; shared over p2p)

- `src/cryptonote_core/sm64_board.{h,cpp}`.
- `cryptonote_core.h:47, 117, 1103-1105` and `cryptonote_core.cpp:716, 1645, 1679`.
- `src/cryptonote_protocol/cryptonote_protocol_defs.h:379, 404`, `cryptonote_protocol_handler.h:98, 147, 152`, `cryptonote_protocol_handler.inl:2652`, `cryptonote_protocol_handler_common.h:46`: the `NOTIFY_SM64_BOARD` message.
- `src/daemon/daemon.cpp:115`: `--sm64-solo` counts as synced.

**Wallet and branding**

- `src/wallet/wallet2.cpp:3490` hard-fork table lookup; `:9658, 9998` output counts with the short unlock window; `:13902` approximate height 0 (block times follow play); `:15942, 15991` the URI scheme `sm64coin:`.
- `src/simplewallet/simplewallet.cpp:2522, 6865`: the `sm64coin:` URI.

## Glossary

- **Run / proof.** The controller inputs a block carries. A proof is a run
  without its poked frames: `varint(frames) || start || codec || body`
  (`proof.rs`).
- **Poke.** A frame whose pad the perturbation replaces with a random one
  (A/B/Z pressed with probability 0.5/0.5/0.2, random stick). Pokes come in
  5 bursts of 2 per 100 ticks of the game clock, drawn from the seed and a
  per-frame digest of the game state. A poke is refunded if the game didn't
  simulate that frame. Pokes are regenerated when verifying, never stored
  (`perturb/src/lib.rs`).
- **Seed.** `cn_fast_hash` of the block hashing blob with the timestamp
  zeroed. Every header field but the timestamp changes the pokes
  (`seed.rs`, `Blockchain::sm64_seed`).
- **Progress / points.** Stars + Bowser keys (held or used) + the Grand Star
  (counted only with 70 stars). A block must add exactly one point, and the
  point must first appear on its last frame (`Rules::is_success`).
- **Cycle.** The playthrough from power-on to the Grand Star. Each block
  continues from its parent's **end state** (the save state after its last
  frame). The Grand Star ends the cycle and the next block starts from
  power-on (`Rules::ends_cycle`).
- **Power-on start / restart.** A run may start from power-on instead of the
  parent's end state. That is free after the genesis block and after a
  Grand Star. Anywhere else it is a **restart**: the run is judged against the
  limit as it stood `restart_delay` seconds earlier (3600 s + 60 s per point
  thrown away, at most 7200 s). So it is valid only that long after the
  parent. This is the escape when nobody can do the next star yet
  (`Rules::restart_delay`, `replay::start_game`).
- **Limit / the auction.** The longest run (in frames) a block may carry. It
  starts low when the parent is stamped and grows every second: 20 difficulty
  halvings below the difficulty's limit, gaining one halving per 60 s, between
  600 frames and 36,000 frames (20 min). The shortest run anyone holds
  becomes valid first (`StarCycle::max_frames`).
- **Difficulty.** Only the time limit: each eighth of a doubling cuts it by 2%.
  Retargeted Monero-style. It does not weigh blocks in fork choice
  (`weigh_by_blocks`).
- **Held block.** A block that is valid at its own timestamp but doesn't fit
  the limit yet by the node's clock. The node keeps it and processes and relays
  it when it fits. This is node policy, not consensus (`hold.rs`,
  `Blockchain::sm64_hold`).
- **Codecs.** 0 raw (4 bytes per pad), 1 run-length, 2 xz: stateless, and the
  block id commits to the decoded run, so they are interchangeable. 3
  `statectx1`, 4/5 `statemix`: state-aware, coded given the game state each
  input is played in, decoded during the replay, canonical, and the block id
  commits to the bytes (`proof.rs`, `statecodec.rs`, `pow/statecodec`).
- **proof_id.** What the block id commits to for the proof (`proof_id.rs`).
- **Rebuild.** Replaying already-accepted blocks (difficulty 0, no limit) to
  recover an end state that isn't cached.
- **Game build.** The exact headless WASM build of Mario 64 that consensus
  runs (`GAME_BUILD`). A different build is a hard fork. v0.4's is sm64-binds
  2.1.2 (`4a657a09...`); v0.3's was 2.1.1 (`897378b4...`).

## Safe to skip

- **Generated or vendored code:** `perturb/src/sm64_addrs.rs` (generated by
  `pow/tools/gen_sm64_addrs.py`), `statecodec/src/trig.rs` (a sine table),
  and the sm64-binds crate (the emulator, from crates.io).
- **Model internals:** `statecodec/src/statectx.rs` and `statemix.rs` are
  compression. Their consensus contract is small: integer arithmetic only,
  and a canonical encoding that `replay_state` re-checks. Read `range.rs` and
  `codec.rs`, and skim the rest.
- **Research tooling:** the bottom of `sm64pow/src/statecodec.rs`
  (`Run`, `state_after`), `sm64pow/examples/`, `sm64pow/tools/`,
  `pow/tools/auction` (the auction model), `pow/perturb/DESIGN.md` (the
  design study behind the pokes).
- **Miner and search** (secondary): `pow/miner` (the AutoMiner and the RRT
  search), `pow/sdk`, `pow/webminer`, `pow/perturb-wasm`, `pow/leaderboard`,
  `pow/launcher`, `python/`. They use `sm64pow::mining` and never decide
  validity.
- **User interface:** `web/` (explorer, browser miner, wallet), `gui/` (the
  Qt wallet).
- **Everything in `monero/` without an `SM64COIN` marker**: stock Monero
  v0.18.5.4.

## Checking that a change keeps consensus

- `cd pow && SM64COIN_ROM=/path/to/baserom.us.z64 cargo test --release -p sm64pow -p sm64coin-perturb -p sm64coin-statecodec`.
  Without the ROM the replay tests skip silently.
- The proof_id vectors: `web/wallet-wasm/test/` (`make-vectors.py`,
  `pow/sm64pow/examples/proofid_vectors.rs`, `vectors.jsonl`).
- Re-verify the live chain: fetch every block with `get_block`
  (`sm64_played: true`) and feed `sm64pow_verify_block` the id, parent, gap,
  `sm64_seed_hash`, difficulty and proof. The verdicts, `proof_id`s and
  decoded pads must not change.
