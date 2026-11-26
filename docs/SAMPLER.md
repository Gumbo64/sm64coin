# Branching sampler: counterfactual rollouts for world-model data

The chain is a narrow dataset: every block is a run that reaches the next star, and (v0.4) the archived
proofs of a height are other runs that reached the same objective. A world model (and a model that stitches
segments or guides search) also needs to see what happens when the player does something else. The sampler
takes states **on the chain** (block boundaries and states along winning runs and archived proofs) and plays
K counterfactual rollouts of H frames from each in the emulator, with an explicit, recorded sampler, writing
`(state features, action, next state features)` and **coverage metrics per sampler** so that the bias of each
is measured, not assumed.

```
cd pow
export SM64COIN_ROM=/path/to/baserom.us.z64
target/release/sm64coin-export export --rpc http://127.0.0.1:16481 --out out/        # or a snapshot directory
target/release/sm64coin-export sample out/ --out sample/ --states 100 --branches 4 --horizon 90 --stride 120
```

Options: `--samplers uniform,objective,random,jitter` (default all four), `--states N` start states per sampler,
`--branches K`, `--horizon H` (frames per branch), `--stride S` (spacing of the candidate start states along
every segment), `--seed X`, `--from-height A --to-height B`, `--eps E` (objective sampler's random share),
`--keep-cutscenes` (by default states in a Mario cutscene action are skipped as start states: the game ignores
the pad there). The input is an export directory or an unpacked snapshot; the cycles are replayed from
power-on (hash-checked against the export, like `verify`), so no start states need to be stored. Same seed, same
output byte for byte.

## The samplers

A sampler is a pair: which start states to branch from, and which actions to play.

| sampler | start states | actions |
|---|---|---|
| `uniform` | spread evenly over position cells (Scattershot-style): round-robin over the cells the candidates are in, a random candidate within a cell | random holds: every 4-24 frames a random stick (uniform direction, magnitude from {0, .35, .65, 1}) and A / B / Z each with their own probability |
| `objective` | random among states in the level the segment ends in (so the objective is reachable) | greedy policy toward the segment's end position (where the star or key was taken): the stick that makes the game's `intendedYaw` point at it (`atan2s(-stickY, stickX) + cameraYaw`), A while the target is above, with probability `eps` per hold a short random hold instead |
| `random` | random | **pink (1/f) noise**: eight octave generators per channel, on stick x, stick y and (thresholded) A, B, Z |
| `jitter` | random | the segment's own next H pads with random-hold windows spliced in: each 8-frame window is replaced with probability `rate`, and `rate` grows with the branch (0 for branch 0, so branch 0 is the chain's own continuation, to 0.6) |

The action space is the one the chain's miners and the perturbation use: the stick and A, B, Z. (The chain's
own pads in `jitter` may also contain other buttons, e.g. Start; they are recorded as they are.) The
objective sampler needs the segment's end position, so it works on export directories (`end_pos`,
`end_level_num` of the block and record metadata); it is a *simple* policy, not a search: it knows nothing of
walls, and the miner's tree search (`obrrt`) could replace `Objective` through the `Policy` trait. The
distance to the segment's end position is measured for **every** sampler on the states where it is defined,
so how much each one is biased toward the objective is a comparison (below).

Start states are chosen in a first pass over candidates (frame 0 and every `--stride` frames of every block
and archived-proof segment); selection uses only the position cell of the candidate (level, area, 256-unit
x/z cell, 128-unit height band) and the sampler's rule, with a per-sampler seeded generator. Each sampler
gets its own `--states` start states, `--branches` branches each. Every trajectory's RNG seed is derived from
(seed, sampler, segment, frame, branch) and recorded.

## Output

```
sample/
  manifest.json              settings, array schemas, game build, source manifest checksum, coverage per sampler
  <sampler>/states.bin       T x (H+1) x 27 little-endian u32: the state features (the export's features.schema.json
                             columns; f32 columns are bit patterns). Record 0 = the start state, record t+1 = after action t
  <sampler>/actions.bin      T x H x 8 bytes: button u16 (the pad's 16 bits, A 0x8000 B 0x4000 Z 0x2000), stick_x i8,
                             stick_y i8, stick_class u16, button_class u16
  <sampler>/trajectories.jsonl   one line per trajectory (array order): sampler, segment {kind block|record, height,
                             name, cycle}, frame_offset, branch, seed, start_state_hash, start_cell, jitter_rate,
                             objective {target, distance_start, distance_end, distance_min}
  <sampler>/coverage.json    the metrics below
```

```python
import json, numpy as np
sch = json.load(open("out/features.schema.json"));  man = json.load(open("sample/manifest.json"))
H = man["settings"]["horizon"]
S = np.fromfile("sample/random/states.bin", dtype=eval(sch["numpy"])).reshape(-1, H + 1)    # [T, H+1] records
A = np.fromfile("sample/random/actions.bin", dtype=eval(man["arrays"]["actions.bin"]["numpy"])).reshape(-1, H)
# transitions: (S[:, :-1], A, S[:, 1:])
```

**Exact input class.** `stick_class` is the class of the stick among **20,129** from the input-analysis tables
(sm64-binds `input-analysis`, `stick_intended.bin`, vendored in `pow/export/data/`): the 65,536 raw sticks
partitioned by what `intendedMag`, `intendedYaw` (relative to the camera) and `INPUT_NONZERO_ANALOG` can tell
apart (proven exhaustively there; (5,3) and (0,0) are one class, (127,127) and (52,52) are one class).
`button_class` is `button & 0xFF3F`, the bits the game reads. A model that should not waste capacity on
differences the game cannot see can predict `(stick_class, button_class)` instead of the raw 16 bits. (This is
the `intended` partition; actions that read the processed stick or the raw stick, such as poles, the pause
menu or the owl, can distinguish more, see that document's tables.)

Sizes: 108 bytes of features per frame plus 8 of action, so a trajectory of 90 frames is ~10.5 KB and
`--states 100 --branches 4 --horizon 90` is ~4.2 MB per sampler (gzip compresses the states well).

## Coverage metrics (per sampler)

`coverage.json` and `manifest.json` `coverage`: `trajectories`, `frames`; **cells** (level, area, 256-unit x/z
cell, 128-unit height band): `cells_visited` by rollout states, `cells_new_vs_chain` (not visited by any chain
segment, sampled every 8 frames: how much the sampler goes beyond the data), `cells_start_states`;
`floors_visited` (distinct (level, area, height band): a proxy for floors, the features do not carry the
floor surface); `levels` (frames per level); `stick_classes_used` of 20,129; histograms of the **input
actions**: `buttons_hist` (button combinations), `stick_region_hist` (neutral, or 8 directions x low/mid/high
magnitude); of Mario's **actions**: `mario_action_group_hist` (stationary, moving, airborne, submerged,
cutscene, automatic, object: `ACT_GROUP_MASK`), `mario_actions_distinct`, `mario_actions_top`;
`entropy_bits` of the cell, button, stick-region and action-group histograms; and `objective` (trajectories
with a target, `mean_distance_start` / `_end` / `_min` to the segment's end position, `fraction_closing`: share
of trajectories that end closer than they started).

### Numbers

v0.2 fixture, blocks 1-16 (55,735 frames, 2,785 chain cells), `--states 100 --branches 4 --horizon 90 --stride 120
--seed 1` (348 candidates; 400 trajectories = 36,000 frames per sampler; about 16 s on one core):

| sampler | cells | new vs chain | floors | stick classes | Mario actions | cell entropy | objective: closing, mean distance start -> end |
|---|---:|---:|---:|---:|---:|---:|---|
| uniform | 2,117 | 1,583 | 266 | 994 | 91 | 9.60 | 43 %, 4,655 -> 4,655 |
| objective | 2,351 | 1,813 | 314 | 1,346 | 99 | 9.98 | **67 %**, 4,663 -> 4,385 |
| random (pink) | 2,274 | 1,702 | 275 | 13,725 | 95 | 9.88 | 51 %, 4,566 -> 4,596 |
| jitter | 1,945 | 1,113 | 234 | 2,572 | 89 | 9.64 | 57 %, 4,608 -> 4,599 |

What it shows: every sampler leaves the chain's cells (jitter least: it stays near the winning run); the
objective sampler is measurably biased toward the target (67 % closing against 51 % for pink noise and 43 %
for random holds, mean distance -277 against +30 and 0) and also covers the most cells (it travels
purposefully); pink noise uses 13x more distinct stick classes than random holds (it is not a stick-grid
sampler) but presses buttons rarely (`A+B+Z` 1 frame in 36,000, against 176 for holds); random holds give
the widest button coverage. The 43 % "closing" of `uniform` and 51 % of `random` are the null baselines for the
objective figure. Smaller than hoped: the objective policy is a greedy heading, so it reaches the objective's
neighbourhood only when the way is open.

v0.4 testnet fixture (2 blocks + 2 archived proofs, 15,418 frames; `--states 40 --branches 4 --horizon 60
--stride 100`): 92 candidates (block and proof segments alike), 587 chain cells, 160 trajectories per sampler
(1.2 MB each), 496-564 cells visited per sampler.

## Tests and limits

`pow/export/tests/v04.rs`: the stick table (20,129 classes, the documented class equalities), pink noise
correlation, hold lengths and the action range, the objective policy's heading against the game's yaw
convention (four camera yaws), determinism (same seed, same bytes; another seed differs), **exact replay**
(a trajectory's recorded actions played from its recorded start state give its recorded states, record by
record), and the measured differences between the samplers.

Limits: a start state is a position on the chain, so the sampler explores around what miners did, not the
whole game; rollouts run the game without perturbation (they are counterfactuals of the *player*, not valid
blocks); cutscene starts are skipped by default; coverage cells ignore the floor surface and objects (stars,
enemies), which the features do not carry; the objective needs the segment's end position (export directories
only).
