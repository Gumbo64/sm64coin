# Two divergences in the chain export: causes and fixes

Found by `sm64coin-export` on the v0.2 mainnet fixture (34 blocks, 2 cycles). Both are explained.

1. Stock Mupen64Plus vs the port: a **port bug** (the game build the chain runs), not a Mupen or timing
   artefact. Consensus-relevant (see "What it means for the chain").
2. The portable save state not exact for blocks 11 and 32: a **translator bug** in sm64-binds 2.1.1,
   already fixed in sm64-binds >= 2.3.1 (commit 5b7bfd2). The game is not involved.

## 1. Cycle 0 does not replay on an N64 after frame 58,532

### Result (three-way)

| pair | cycle 0 (107,931 frames) | cycle 1 (35,574) | 4 TAS movies (control) |
|---|---|---|---|
| port (pinned build 897378b) vs sm64_physics (`sm64-binds/oracle`) | first difference **after input frame 58,533** (RNG), objects from 58,532 | identical | identical (all four, 8,772 / 23,248 / 128,808 / 140,467 frames) |
| port vs Mupen64Plus 2.6.0, every poll (Mario, RNG, timer) | first difference at poll 58,536: RNG; Mario follows later | 0 of 35,574 polls differ | not run (the oracle's own claim: sm64_physics == Mupen) |
| sm64_physics vs Mupen64Plus, cycle 0 | RNG at poll 58,535 is 0xcf6d in both | - | - |

So the port is the odd one out; Mupen and sm64_physics agree. The same Mupen run with the port **fixed**
(below): 0 of 107,930 polls differ, and the oracle reports "identical for all 107,931 frames". The earlier
"matches exactly up to frame 77,400, then diverges" was an artefact of how it was looked at: the RNG
differs from poll 58,536 on, but Mario is only affected when something he does consumes the drifted RNG
(here around block 19, frames 77,400-77,500). Everything between is the same Mario with a different
random-number stream.

### First diverging frame and cause

Cycle frame **58,532** (block 17, frame 2,797 of that block; the `GlobalTimer` is 58,534 while it is
played), Mario splashing in Tall Tall Mountain's water (level 12). The object pool is nearly full
(237 of 240 objects). That frame the game spawns objects like this (a trace compiled into a scratch copy of
the port; slot = index in `gObjectPool`):

```
alloc -> slot23   by Mario                      (a bhvPlungeBubble)
alloc -> slot181  by slot23, alloc -> slot172   (bubbles)
alloc -> NULL     by slot23: pool full, evict the head of the unimportant list (slot187), reuse it
alloc -> NULL     by slot180 (a bhvSmallWaterWave above the water): pool full, evict slot162, spawn a splash
```

The last allocation must not happen. `bhv_small_water_wave_loop` (water_objs.inc.c) and
`bhv_object_bubble_loop` (bubble.inc.c) guard the splash spawn like the N64 original does,

```c
if (gFreeObjectList.next != NULL) spawn_object(o, MODEL_SMALL_WATER_SPLASH, bhvObjectWaterSplash);
```

but the port wrote the guard as `#ifdef USE_SYSTEM_MALLOC  TRUE  #else gFreeObjectList.next != NULL`. The
headless/web builds define `USE_SYSTEM_MALLOC` (Makefile) while using the N64's fixed 240-object pool
(`object_list_processor.c`: "Objects come from the N64's fixed pool even with USE_SYSTEM_MALLOC"), so the
guard is always true, the allocation fails, and `allocate_object` evicts one more unimportant object
(an in-flight water droplet) than the N64 does and creates a splash the N64 never creates. The next
`random_float()` consumers see a different object list and the RNG streams part (0xf460 vs 0xcf6d at
timer 58,535). `try_to_spawn_object` already uses `MALLOC_OBJECTS` for the same guard, which is how the
earlier 240-pool fix (tas/README.md) missed these two sites.

It is not a float/FPU, lag-frame, polling, audio/DMA or null-video effect: Mupen's poll-by-poll Mario
matches the fixed port exactly for the whole cycle, and the m64s contain no reserved-bit patterns
(`0x00C0`, console reset) in either cycle.

### Fix (a game change: do not apply casually)

`docs/divergence/pool-full-splash.patch` (two lines; `USE_SYSTEM_MALLOC` -> `MALLOC_OBJECTS` in both
guards). Verified in a scratch rebuild of sm64-port: cycle 0 and cycle 1 are `identical` to sm64_physics
over all frames, and `threeway.sh` shows 0 differing polls against Mupen64Plus for the whole of cycle 0.
The 4 TAS movies never fill the pool in water, so they are unaffected.
Other `USE_SYSTEM_MALLOC` sites do not concern the object pool (display lists, main pool, warp node NULL
guard) except the surface pools in `surface_load.c` (N64: 2,300 surfaces / 7,000 nodes, here unbounded),
which only matters on a level that overflows them; worth an audit, nothing observed.

### What it means for the chain

* The chain is self-consistent (every node runs the same port); only fidelity to the N64 is affected.
* Any run that fills the object pool while water bubbles/waves splash would not replay on a real N64 after
  that point (it is not "one block": from there the RNG stream differs). It occurred once in 143,505 frames
  here; water levels with many bubbles and splash particles are the risk. With the TAS-style play of the
  fixture it is rare, with miners that dive and splash at random it will be less rare.
* The fix changes the game, so it is a new game build id (`GAME_BUILD`) = a hard fork. v0.3 starts a new
  chain, which is the natural place. Data exported from a v0.2 chain is N64-exact only up to the first
  frame where this triggers; `threeway.sh` finds that frame for any export.

### Reproduce

```
cd pow && cargo build --release -p sm64coin-export
export SM64COIN_ROM=...  SM64COIN_EXPORT=$PWD/target/release/sm64coin-export
target/release/sm64coin-export export --jsonl sm64pow/tests/fixtures/mainnet_v02.jsonl --out out --mupen64plus
export/tools/mupen64plus/threeway.sh out 0 /tmp/tw     # port vs Mupen, every poll: 49,394 of 107,928 polls differ
# with the oracle (sm64-binds/oracle, built against a local sm64_physics checkout):
ORACLE=.../oracle SM64_PHYSICS_DIR=... export/tools/mupen64plus/threeway.sh out 0 /tmp/tw
# the cycle itself is checked in: docs/divergence/cycle_0000_v02.m64
```

`threeway.sh EXPORT CYCLE WORKDIR` is the reusable check: it replays the cycle's m64 in stock Mupen64Plus,
dumps Mario, RNG and timer from RDRAM at **every** poll (`m64_input.c`, `M64_DUMP`; the core's save-state
request only completes for the first few requests of a run, which is why the earlier check saw one state per
5,000 frames), compares them with the port (`sm64coin-export compare-mupen-dump`) and, if configured, also
runs the oracle. `cargo run --release --example divergence -- CYCLE.m64 DUMP.bin pool` prints per-word
differences of Mario and the object pool (M64_DUMP without `M64_DUMP_NOPOOL`).

## 2. Portable start states of blocks 11 and 32

### What is wrong

A game loaded from the portable start state of block 11 (or 32) and fed the block's pads diverges from the
same game loaded from the raw state: first in object slot 64 (a `bhvSmallPenguin`, Snowman's Land) at
frame 2,304 of block 11 (3,827 of block 32: gObjectPool, then RNG at 3,828, camera, Mario), `gMatStack`
aside. Reproducer: `cargo run --release --example portable_diff -- out/blocks/h0000011_e43083fa` (needs an
export with `--states both`; `FRESH=1` re-exports the portable state with the linked translator, `DETAIL=1`
prints differing object words, `MEMDIFF=<frame>` the differing data/bss words by symbol). Between the two
games nothing else differs at frame 2,303: not the variables, not Mario, not the RNG.

Cause: the translator (sm64-binds 2.1.1) treated every `rawData` word of an object as a possible pointer
and relocated any value that fell in the address range, so a number that merely looks like an address was rewritten
(the changelog of 5b7bfd2 lists signs' `oInteractType = 0x800000`, a Boo's oscillation timer, spinning yaws,
`oBehParams`; which word of the penguin in slot 64 it is here was not pinned down, the bisect below is the
evidence). The game later used the rewritten number. Fixed in sm64-binds commit 5b7bfd2
("An object's rawData words are pointers only in slots its behavior uses as pointers"); bisected: with the
translator of 23d4dfd (2.3.0) both blocks still differ, with 5b7bfd2 and every later commit (up to 2.3.2,
branch portable-fixes) block 11 is identical for all 2,635 frames and block 32 for all 4,701 frames,
comparing GameState, RNG, Mario and the digest of **every portable variable on every frame**.

### Fix for SM64Coin

Bump the export crate's translator to sm64-binds >= 2.3.1 (the game build is the same, 897378b; it runs
under the same `GAME_BUILD` check). Until the pin moves, `start_portable_exact` is false on those two blocks
and the raw state is the fallback (as documented). `tests/export.rs::portable_states_of_all_fixture_blocks`
(`--ignored`) accepts either result and fails on any other block.

### Verification beyond the two blocks

See the table below (portable_fuzz, sm64-binds branch portable-chain-exact): translate-and-continue at
every block boundary of both chain cycles.

