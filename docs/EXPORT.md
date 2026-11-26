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
(per-block start states), `--clear-reserved-bits` (clear the reserved button bits 0x00C0 in the cycle `.m64` files: the game ignores them, Mupen64Plus resets on a sample with both set), `--mupen64plus` (also write a variant for stock
Mupen64Plus, see below), `--format v0.4|legacy` (the chain's rules: **v0.4**, the
default, replays committed-input proofs and exports included proofs; `legacy` for v0.2 / v0.3 dumps),
`--record-m64` (v0.4: one movie per archived proof, see below).

The input must be one chain in height order. It may start at any block whose
proof starts from power-on (height 1, the block after a Grand Star, a
restart); a block that continues from its parent needs the parent in the input.
JSONL lines: `{height, id, prev, ts, seed, proof}` (hex strings), optionally
`difficulty`, `miner_key`, `sig`, and (v0.4) `records` (the block's trailing
`sm64_proofs` field, hex), `payout` (`flag || spend || view`, hex), `proofs_root`. `seed` is
`sm64_seed_hash`. From v0.3 the RPC path also fills `sm64_sig` (from the block blob) and the miner key
(from the coinbase extra nonce); on v0.4 it also takes `sm64_proofs` from the block JSON and the
payout address and included-proofs root from the extra nonce's TLVs.

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
   every run needs a fresh `HOME` (the script does that). `threeway_m64.sh MOVIE.m64 WORKDIR` does the
   per-poll comparison for any power-on movie (a cycle's or an archived
   proof's), which is what `verify-snapshot --mupen` uses.

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

## v0.4: committed inputs and included proofs

On a v0.4 chain a block's proof is a **committed** blob (header `varint(frames) || start || input hash`,
then any codec: state-coded ones are decoded by replaying), and block h+1 carries up to 64 **included
proofs**: other miners' valid signed runs for height h (docs/V0.4-DESIGN.md section 2), from the same
start as block h, within 60 s of auction time after the winner. They are exactly the counterfactual
data the chain keeps: runs that did not win but reached the same objective by other inputs. The exporter
replays them with the same consensus code (`chain::played_committed_block`) and exports each as a
**segment from the height's natural start**, like a block:

```
records/h0000001_00_c6406f28.json         metadata (below)
records/h0000001_00_c6406f28.pads         effective inputs, 4 bytes per frame (pokes from the proof's own seed applied)
records/h0000001_00_c6406f28.features.bin with --features
records/h0000001_00_c6406f28.m64          with --record-m64: a movie from power-on (the cycle's frames up to the
                                          block, then the proof's), playable in Mupen64Plus
```

Name: `h<height>_<index in the block's records>_<first 4 bytes of the input hash>`. Record metadata:
`height` (the height the run is a proof for), `index`, `included_in_height` / `included_in_id` (block
h+1), `input_hash`, `seed`, `miner_key`, `payout` (the payout address its template commits: `flag ||
spend key || view key`, flag 1 = subaddress), `sig`, `proof_codec`, `proof_bytes`, `frames`,
`poked_frames`, `poked_mask_hex`, `start_kind` (`power_on` | `parent`), `start_state_hash`,
`start_same_as_block` (true: the height's block's `.start.*` files are this segment's start state),
`counted_for_weight` (whether it counted for fork choice: its frames within the weight window,
`records::PROPOSED.weight_seconds` of auction time after the winner's due time at the block's
difficulty; null when the dump has no difficulty), `frames_vs_winner`, `progress_after`, `event`,
`course`, `star`, the game's end state (`end_*`, `end_state_hash`), `cycle`, `m64_frame_offset`,
`files`. The block's own metadata gains `payout`, `proofs_carried` (records in this block, for the
parent's height) and `proofs_archived` (records of this height, taken from the next block; null for
the tip, whose proofs are not on chain yet).

The same checks as for blocks: each proof's start state is rebuilt (power-on, or the previous block's end
state), the plain replay of its effective pads must end in the chain replay's RAM image (`MISMATCH`
otherwise), and `verify` replays every record from its start and compares the end hash.
`manifest.json` has `schema_version: 2`, `rules`, `records`, `record_files` (the list of record metadata
files). Not exported: the proofs of the **tip** height (no next block yet).

**The v0.2 fixture on the v0.4 game build.** The v0.2 mainnet fixture (`sm64pow/tests/fixtures/mainnet_v02.jsonl`)
replays with the v0.4 game build (sm64-binds 2.1.2) only through block 16: block 17 does not reach its star
any more (the build changed game behaviour that the old chain depended on), so export it with
`--format legacy --max-blocks 16`, or with the v0.2-era binary for the whole fixture. v0.4 data is
unaffected. The v0.4 test data (`pow/export/tests/fixtures/v04_testnet.jsonl`, two real testnet blocks, the
second with two included proofs) exports, verifies and passes the three-way check on this build.

## Snapshots

A **snapshot** is an export plus a versioned manifest, checksums and an optional deterministic tarball:

```
cd pow
# from a node (reads get_info, get_block; nothing is submitted) or a JSONL dump
target/release/sm64coin-export snapshot --rpc http://127.0.0.1:16481 --out snap/ --features --record-m64 --tar snap.tar.gz
target/release/sm64coin-export verify-snapshot snap.tar.gz --mupen auto --mupen-records 2
```

`snapshot` takes the options of `export` (`--states` defaults to `portable`) and adds `snapshot.json`,
`SHA256SUMS` and `README.txt`; `--tar FILE` writes a `.tar.gz` (sorted entries, mtime 0, owner 0: the same
snapshot is always the same bytes; one top-level directory named like `--out`). `SOURCE_DATE_EPOCH` fixes
`created_utc`.

`snapshot.json` (`snapshot_version` 1, `schema_version` 2 of the layout):

| field | |
|---|---|
| `dataset`, `snapshot_version`, `schema_version`, `created_utc`, `exporter` | what and when |
| `chain` | `network` (`mainnet`/`testnet`/`stagenet`/`unknown`), `rules`, `genesis_id`, `node`, `node_height`, `node_top_block`, `first_height`, `last_height`, `first_block_id`, `last_block_id`, `block_ids_sha256` (sha256 over the block ids in height order: two snapshots of one chain agree on a common prefix) |
| `game` | `build` (`GAME_BUILD`: state hashes are specific to it), `rom` (name, crc1) |
| `counts` | `blocks`, `cycles`, `frames`, `poked_frames`, `records`, `record_frames`, `records_counted_for_weight`, `features`, `record_m64`, `files`, `bytes` |
| `sums_sha256` | sha256 of `SHA256SUMS` (lines `sha256  size  path`, sorted), which pins every other file |

`verify-snapshot DIR|FILE.tar.gz` checks, in order: the checksums (changed, missing and extra files), the
structure (heights contiguous, `prev` chaining, counts and `block_ids_sha256` agree with the files), the game
build (refuses with a clear error if it is not this binary's), then **replays everything**: every cycle from
power-on without pokes and every archived proof from its start, comparing every recorded state hash (so a
changed pad anywhere is found), and with `--mupen auto|WORKDIR` the **three-way check** (port vs stock
Mupen64Plus at every controller poll: Mario's action, position, velocity, health, stars, the RNG word and the
global timer) of every cycle and, with `--mupen-records N`, N archived proofs (needs `--record-m64`). The
three-way check runs where the rig is available (libmupen64plus2, mupen64plus-rsp-hle, libmupen64plus-dev,
gcc; `tools/mupen64plus/threeway_m64.sh`, found via `--mupen-tools`, `$SM64COIN_MUPEN_TOOLS` or the checkout)
and is reported as `skipped: why` otherwise. `--no-replay` stops after the structure; `--report FILE` writes
the result as JSON. Exit code 2 on any problem.

The workflow `.github/workflows/dataset-snapshot.yml` runs `snapshot` and `verify-snapshot` against a node
URL (manual dispatch with inputs; optional weekly schedule, off until the repository variable
`SNAPSHOT_SCHEDULE` is `on`) and uploads the tarball, `snapshot.json` and the verify report as a workflow
artifact (30 days). It needs the repository secret `ROM_URL` (the ROM in private storage, as for the other
ROM tests) and the variable `SNAPSHOT_NODE_URL` for scheduled runs.

### Publishing a snapshot

Left to the maintainer; the snapshot is one file (`.tar.gz`) plus its `snapshot.json`, so any of these work:

- **GitHub release asset** (simple, 2 GB per file): `gh release create data-h1234 sm64coin-snapshot-*.tar.gz sm64coin-snapshot-*.snapshot.json`
  from the downloaded workflow artifact (or an extra workflow step with `contents: write`).
- **Hugging Face dataset** (browsable, versioned, large files): `huggingface-cli upload <org>/sm64coin-chain
  snap.tar.gz` or upload the unpacked directory; a dataset card can quote `snapshot.json`.
- Anywhere else that serves a file; consumers check `sums_sha256` from `snapshot.json` and run `verify-snapshot`.

The ROM is never part of a snapshot: users replay with their own US ROM (the m64s and pads are the data; the
states are rebuilt by the game).

Sizes (the two-block v0.4 testnet fixture, 7,603 frames of blocks, 2 archived proofs of 7,815 frames,
`--features --record-m64`): 21 files, 1.99 MB unpacked, 683 KB `.tar.gz`; features are 108 bytes per frame and
dominate (without them: about 0.3 MB for the same chain). Portable start states are ~25-160 KB gzipped per segment.

## Limits

- The ROM must be the US ROM; the game build is pinned (`GAME_BUILD`).
- Features are read for every frame of every block: a 36,000-frame block is
  ~4 MB of features. Use `--features` only when needed.
- Chain replay uses no time limit (`rebuild_block` semantics): the export
  exports what the chain accepted, it does not re-judge it.
- v0.3 RPC parsing (`sm64_sig`, miner key) follows `web/src/lib/replay/core.js`
  and is covered by a unit test on a synthetic block; it was not run against a
  live v0.3 node (none was available).
