# Chain-to-dataset export

`sm64coin-export` (`pow/export`) turns SM64Coin blocks into training data in
the standard Mupen64 `.m64` format. It changes no consensus code: blocks are
replayed with `sm64pow::chain::played_wanted_block`, the function the daemon
uses for `get_sm64_cycle`.

## What the chain stores, and what the game read

A block's proof holds only the player's inputs on frames the perturbation did
**not** poke. On a poked frame the perturbation replaces the **whole pad**
(A/B/Z random, stick random, every other button released) with numbers drawn
from the block's seed. The export replays each block (start state = parent's
end state, or power-on) and records the pad the game *actually read* on every
frame: the **effective inputs**. Played from the same start state without any
perturbation, they reproduce the chain exactly (see [Verification](#verification)).

## Usage

```
cd pow
export SM64COIN_ROM=/path/to/baserom.us.z64          # US ROM, big-endian .z64
cargo build --release -p sm64coin-export

# from a daemon (read-only RPC: get_info, get_block) ...
target/release/sm64coin-export export --rpc http://127.0.0.1:16481 --out out/ --features --states portable
# ... or from a JSONL dump (the sm64pow test fixtures' format)
target/release/sm64coin-export fetch  --rpc http://127.0.0.1:16481 --out chain.jsonl
target/release/sm64coin-export export --jsonl chain.jsonl --out out/ --features --states both --mupen64plus

target/release/sm64coin-export verify out/             # replay every cycle m64 from power-on, no pokes
```

Options: `--from H --to H` (RPC or JSONL range), `--max-blocks N`,
`--features` (per-frame state features), `--states none|portable|raw|both`
(per-block start states), `--mupen64plus` (also write a variant for stock
Mupen64Plus, see below).

The input must be one chain in height order. It may start at any block whose
proof starts from power-on (height 1, the block after a Grand Star, a
restart); a block that continues from its parent needs the parent in the input.
JSONL lines: `{height, id, prev, ts, seed, proof}` (hex strings), optionally
`difficulty`, `miner_key`, `sig`. `seed` is `sm64_seed_hash`. From v0.3 the
RPC path also fills `sm64_sig` (from the block blob) and the miner key (from
the coinbase extra nonce).

## Output

```
out/
  manifest.json                    format, game build, ROM, totals, every cycle with its blocks
  features.schema.json             column schema of the *.features.bin files (with a numpy dtype)
  cycles/cycle_0000.m64            (b) effective inputs of a cycle, from power-on
  cycles/cycle_0000.json           blocks, frame boundaries, end-state hashes
  cycles/cycle_0000.mupen64plus.m64   with --mupen64plus: one leading zero poll
  blocks/h0000012_<id8>.json       (c) metadata of one block (below)
  blocks/h0000012_<id8>.pads       (a) effective inputs of the block, 4 bytes per frame, no header
  blocks/h0000012_<id8>.features.bin  (d) with --features
  blocks/h0000012_<id8>.start.portable.gz   with --states portable|both
  blocks/h0000012_<id8>.start.state.gz      with --states raw|both
```

A **cycle** is a run of blocks from one power-on start to the next (a Grand
Star ends the cycle; a *restart* power-on mid-cycle abandons it, and the
abandoned blocks stay on chain, so they are a cycle of their own with
`end: "restarted"`). The last cycle of a dump that stops early has `end: "open"`.

### Frames and the m64 format (a, b)

One frame = one `step_game` = one controller poll = 1/30 s of game time. A
frame is 4 bytes: the pad's 16 button bits **big-endian** (A is bit 7 of byte
0, B bit 6, Z bit 5, Start bit 4, then the D-pad, then byte 1: C buttons, L,
R), then stick x and stick y as signed bytes. That is `GamePad::button.to_be_bytes()`
plus the stick, the layout of sm64-binds' `tas/*.m64` and of Mupen's `BUTTONS`
(read as a little-endian u32).

The m64 header is 0x400 bytes, version 3: signature `M64\x1a`, uid = the
cycle's first block timestamp, `vi_count` = 2 x samples, 60 VI/s, 1 controller,
`samples` = frames, start type 2 (power-on), ROM name/CRC1/country from the
ROM, author and description (which blocks). Plugin names are placeholders.

### Block metadata (c)

`height, id, prev, timestamp, seed, difficulty, miner_key, sig` (miner key and
signature are null on v0.2 blocks), `start_kind` (`genesis`, `cycle_start`,
`restart`, `power_on`, `parent`), `proof_codec`, `proof_bytes`, `frames`,
`poked_frames`, `poked_mask_hex` (bit k, LSB first within each byte, set when
frame k was poked), `progress_before` / `progress_after` (`stars`, `keys`,
`grand_star`, `points`), `event` (`c09s1` = course 9 star 1, `c00s3` = castle,
`key1`, `grand`), `course`, `star`, the game's end `course_num / level_num /
area_index / act_num / pos`, `start_state_hash` / `end_state_hash`
(`cn_fast_hash` of the whole 16 MB RAM image; a block's start hash is its
parent's end hash), `cycle`, `cycle_frame_offset` (where the block starts in
the cycle's m64) and the file names.

### Per-frame features (d)

`*.features.bin`: `frames + 1` records of 27 little-endian 32-bit words. Record
0 is the state before frame 0, record k the state after frame k-1 was played
(so the last record equals the first of the child block). Columns, dtypes and
a numpy dtype string are in `features.schema.json`: global timer, Mario action,
previous action, flags, action state/timer, input flags, intended yaw/magnitude,
face yaw, camera yaw, position, velocity, forward speed, health, coins, stars,
level, area, the game's RNG word, camera position. They come from the same RAM
regions the perturbation reads (`pow/perturb/src/sm64.rs`), plus float fields at
the `MarioState` offsets; the camera position is checked against
`get_game_state().lakitu_pos` in the tests.

```python
import json, numpy as np
sch = json.load(open("out/features.schema.json"))
a = np.fromfile("out/blocks/h0000002_41120ad5.features.bin", dtype=eval(sch["numpy"]))
```

## Start states of the segments

Each block's segment starts from the parent's end state (power-on for cycle
starts). Formats that exist:

| format | what | file |
|---|---|---|
| **portable** (`SM64PORT`) | sm64-binds' build-neutral state: the game's variables and reachable heap blocks with pointers made neutral. ~25 KB gzipped. Loads in the headless game (`SM64Game::load_portable`) and in the rendering/browser build (`engine.load_portable_state`). | `.start.portable.gz` (gunzip, `Portable::from_bytes`) |
| **raw** | the headless game's whole `SaveState` (16 MB, ~2.4 MB gzipped). Exact, headless build only (the pinned game build, see `GAME_BUILD`). | `.start.state.gz` |
| Mupen `.st` | **not possible.** sm64-binds can *read* Mupen64 states (`MupenState`, `load_mupen_state`) but the port is the decomp compiled to wasm32, not an N64 emulator: it has its own memory layout, malloc heap and no ROM segments, so there is nothing to write RDRAM, CPU registers, RSP and TLB state from. A "start from snapshot" m64 therefore cannot be made. | |

For Mupen64 use, the way to start mid-chain is the cycle m64 from power-on (the
whole prefix is replayed). The portable state is verified per block at export
time (`start_portable_exact`): a fresh game loaded from it and fed the block's
pads must end in the chain's end state (same `GameState`, progress, and the
same portable variable digest; `gMatStack`, the render walk's scratch matrices,
is ignored). On the 34 v0.2 fixture blocks this holds for 32; blocks 11 and 32
do not reproduce from the portable state with the pinned translator (sm64-binds 2.1.1; see DIVERGENCE.md), so
`start_portable_exact` is false there and the raw state must be used for them.

## Verification

1. **Pokes touch only the pad (every block, at export).** The block's start
   state is loaded into a fresh game and the effective pads are stepped with
   plain `step_game`, no perturbation. The resulting 16 MB RAM image must be
   byte-identical to the chain replay's end state. Any difference is printed as
   `MISMATCH` and makes the exit code 2.
2. **Cycle m64s (`verify`).** Reads the written m64 files only, replays each
   from power-on, and compares the RAM-image hash at every block boundary with
   the hash recorded from the chain replay; also checks each `.pads` file
   against its slice of the cycle.
3. **Stock Mupen64Plus (best effort, `tools/mupen64plus/`).** A small front
   end drives libmupen64plus 2.6 (Debian/Ubuntu `libmupen64plus2`,
   `mupen64plus-rsp-hle`) with an input plugin that plays an m64, a null video
   plugin that raises the RDP-done interrupt (the core's built-in dummy video
   does not, and the game stalls), and the built-in dummy audio.
   `threeway.sh EXPORT CYCLE WORKDIR` compares every poll (and, with the sm64-binds oracle, sm64_physics
   too); `check_cycle.sh EXPORT CYCLE WORKDIR` saves states at every block boundary
   and every 5000 frames, and `sm64coin-export compare-mupen` compares Mario
   (action, position, velocity, health, stars) with the port. Notes: Mupen64Plus
   polls once more at boot, hence the `.mupen64plus.m64` variant with one
   leading zero poll; a state requested in poll p is the port after p - 2
   frames for that variant; Mupen keeps the EEPROM save file between runs, so
   every run needs a fresh `HOME` (the script does that).

### Results (v0.2 mainnet fixture, blocks 1-34; the local node's 38 blocks give the same)

- Pokes touch only the pad: **0 mismatches in 38 blocks / 156,417 frames
  (14,652 poked)**. The plain replay of the effective pads ends in a
  byte-identical RAM image at every block. `verify` replays all 3 cycles
  (cycle 0: blocks 1-24, 107,931 frames, abandoned by the restart at block 25;
  cycle 1: blocks 25-34, 35,574 frames) from power-on with 0 divergences.
- Stock Mupen64Plus 2.6.0, compared at **every poll** (`tools/mupen64plus/threeway.sh`: Mario, RNG, timer):
  cycle 1 (35,574 frames) matches the port in all polls. Cycle 0 matches until poll 58,535 and then the RNG
  differs: a **port bug**, found and explained in [DIVERGENCE.md](DIVERGENCE.md): at frame 58,532 (block 17,
  object pool full) two splash guards use `USE_SYSTEM_MALLOC` where the N64 checks the free object list, so
  the port evicts and spawns one object more than the N64. sm64_physics (the oracle) agrees with Mupen, not
  with the port. With a two-line fix of the game (patch in docs/divergence/), the whole cycle matches Mupen
  at every poll. The fix changes the game build (hard fork), so it is not applied in this repository.
- Portable start states: exact for 36 of 38 blocks (34 distinct fixture blocks; 2 cycles); blocks 11 and 32
  do not reproduce under sm64-binds 2.1.1's translator (it typed a stale allocation-pool pointer and rewrote
  reused memory); fixed in sm64-binds >= 2.3.1 (commit e04b17f), see DIVERGENCE.md.

## Limits

- The ROM must be the US ROM; the game build is pinned (`GAME_BUILD`).
- Features are read for every frame of every block: a 36,000-frame block is
  ~4 MB of features. Use `--features` only when needed.
- Chain replay uses no time limit (`rebuild_block` semantics): the export
  exports what the chain accepted, it does not re-judge it.
- v0.3 RPC parsing (`sm64_sig`, miner key) follows `web/src/lib/replay/core.js`
  and is covered by a unit test on a synthetic block; it was not run against a
  live v0.3 node (none was available).
