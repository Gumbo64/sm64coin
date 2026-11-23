# Poke system design (Opus poke-design agent, 2026-11-22; grid 2026-11-23)

Recommendation: mix the state digest into the poke generator once per 100-tick window instead of after every frame; keep everything else (full pad replacement A .5 / B .5 / Z .2, stick uniform [-80,80]^2, 5 bursts x 2 frames per 100 ticks, no draws while the clock stands still, the settle refund). Each window's schedule is then fixed once it starts, but seeing it still needs simulating the run to the window start. ~10 lines, game-agnostic, same in WASM, no verifier cost. Next candidate (untested in the GA): a 200-300 tick freeze at the same density.

Not adopted: coarse digest (no effect on rerolls: 0.81/0.83 rescued, same as full); refund-by-effect (~4x slower verification; the "pending until effective" form locks up menus: 4,063 refunds in 4,329 boot frames; a budget-return variant only measured for cost); gentle shapes (they depend on the wanted pad, so the proof would have to store poked frames' wanted pads, breaking step_with and the format).

## Reroll vs mix interval (fraction of harmful segments a K-nudge reroller rescues; block1 / chomp-a, 12 seeds, from frame 1800)

| freeze ticks | harmless as-is | rescued K=8 | rescued K=32 | reroll sim frames/window (K=32) |
|---|---|---|---|---|
| 1 (today) | 0.77 / 0.63 | 0.63 / 0.77 | 0.81 / 0.82 | 1265 / 1523 |
| 10 | 0.70 / 0.58 | 0.54 / 0.61 | 0.70 / 0.73 | 1218 / 1673 |
| 25 | 0.63 / 0.49 | 0.41 / 0.47 | 0.62 / 0.63 | 1358 / 1888 |
| 50 | 0.53 / 0.39 | 0.36 / 0.33 | 0.55 / 0.43 | 1675 / 2427 |
| 100 | 0.23 / 0.21 | 0.25 / 0.19 | 0.41 / 0.33 | 1914 / 2480 |
| 200 | 0.16 / 0.13 | - | 0.22 / 0.17 | 2143 / 2595 |
| 300 | 0.15 / 0.05 | - | 0.11 / 0.18 | 1959 / 2589 |
| 600 | 0.10 / 0.02 | - | 0.06 / 0.10 | 2183 / 2669 |

## Freezing rewards planning (known full burst 20 frames ahead; 556 cases; GA pop 32 x 12 gens; loss in frames, failure = 90)

| design | harmless untouched | after-only recovered / loss | before+after recovered / loss |
|---|---|---|---|
| full b2 | 47% | 72% / 31.3 | 80% / 23.6 |
| stick-only b2 | 70% | 80% / 21.1 | 82% / 20.9 |

## End to end, RHEA pop 128 / h 600 / commit 1, BoB block 1 from power-on

| mix | attempt 1 | attempt 2 |
|---|---|---|
| once per 100 ticks (agent build, 3 threads) | 3896 f, 1498 s, 129M sim | 3605 f, 1318 s, 106M sim |
| every frame (pow/target/runcloud/ga128.csv, main build, 4 threads) | 3665 f, 972 s, 107M sim | 3621 f, 1011 s, 109M sim |

Same within noise (2+2 runs, different builds). Pending: a no-poke reference and a same-build per-frame control, appending to rh.csv (rh_queue.pid, rh_queue2.pid).

Other: route replay 0/12 at every interval even with 32 rerolls; first window becomes a pure function of the block hash (a fixed plan finds a harmless first window with p~0.2, i.e. a few nonces, under either rule); grinding beyond that ~p^-W in both; verifier ~60k frames/s unchanged.

Risks: timed absorbers (doors/dialogs) onto known window pokes (~40 frames to dodge vs ~9 to recover); first-window grinding grows with freeze length; thin GA evidence.

Reproduce: copy at /workspaces/sm64-combined/pokedesign/ (knobs via env POKE_MIX, POKE_AMOUNT, POKE_COARSE, POKE_REFUND_EFFECT); files here: knobs.patch, pokedesign.rs, pokeplan.rs, attack.tsv, summarize.py, plan16/, plan32/, rh.csv, attack_grid*.sh, rh_queue*.sh.

## 3x3 grid: separate timing and pad streams (2026-11-23)

Two generators: timing (when pokes land) and pad (what they press). Each is mixed per frame, per 100-tick window, or never (per game: a pure function of the block hash). The numbers are the share of harmful segments a K-nudge reroller rescues (lower is better), for block1 / chomp-a, K=8 then K=32. Recorded routes survived replay 0/12 in every cell.

| | pad per frame | pad per window | pad per game |
|---|---|---|---|
| timing per frame | 0.64 / 0.78 ; 0.80 / 0.82 | 0.60 / 0.76 ; 0.78 / 0.80 | 0.63 / 0.74 ; 0.77 / 0.81 |
| timing per window | 0.26 / 0.40 ; 0.51 / 0.62 | 0.19 / 0.20 ; 0.36 / 0.33 | 0.24 / 0.22 ; 0.39 / 0.32 |
| timing per game | 0.29 / 0.42 ; 0.53 / 0.58 | 0.17 / 0.17 ; 0.26 / 0.22 | 0.01 / 0.01 ; 0.02 / 0.01 |

- **Timing is the lever.** While the timing stream is mixed per frame, freezing the pads doesn't help.
- **Grinding a nonce whose pokes leave a fixed plan harmless:**
  - First 5 windows: about 10^3 nonces.
  - First 10 windows: about 10^9 (block1), and none found for chomp-a.
  - A run is 30–65 windows long.
  - Per-game timing adds an emulator-free prefilter: 96–99% of nonces are rejected without simulating, for the first 5 windows. It also lets a miner plan against the whole run's poke timing.
- **Adopted at first: both streams per window** (`timing_mix = pad_mix = 100`); superseded by the compute-normalised comparison below, which adopts per frame. Per-game timing with per-window pads rerolls even less (0.22–0.26) at the cost of whole-run timing knowledge and cheaper early grinding.

## Compute-normalised comparison (2026-11-23): per frame adopted

**The metric:** envelopes of best run length against compute, for a route-imitating RHEA (pop 32, horizon 300; two pop-128 checks), from 600 frames before Bob-omb Battlefield's star (block1-human@3729), 5 seeds per arm. Exploit advantage is the honest envelope minus the exploit's envelope at equal compute (positive means the exploit wins). The chomp-a start failed even without pokes, a harness issue, so it is excluded. Data: `pow/target/pokemetric/grid.csv`, `analyze.py`.

| design | failed | E @1u / @4u (frames) | exploit advantage |
|---|---|---|---|
| no pokes | 0/5 | 580 / 573 | - |
| per frame | 3/5 | 918 / 662 | reroll: -82 / -101 (it loses) |
| per window | 5/5 | 1000 / 1000 (cap) | reroll: +0 / +257 |
| per game | 5/5 | 1000 / 1000 (cap) | best-of-1e5 nonces vs harm map: +414 / +425 (0/5 failed, 586 f) |

**Decision:** mix both streams every frame (`timing_mix = pad_mix = 1`). Under per-frame mixing, honest search is already the best strategy. Freezing the schedule makes dodging a separate exploit:
- per window: rerolls at the window start beat honest search by about 250 frames;
- per game: nonce screening costs about 1% of a run (about 70-100k nonces/s with no emulation) and erases the pokes. Per game would need a seed derivation of at least 0.1-1 s per nonce.

**Open:**
- At today's strength, the poke tax on this hard stretch is mostly failures (per frame: 3/5 failed, sd about 190 frames). Tune the rate and strength with this metric, over more start states.

## Rate and strength sweep (2026-11-23): 5 bursts of 2 kept

Route-imitating RHEA (pop 32, horizon 300, one thread, cap = 600 + 400 frames) from 600 frames before the star, per-frame mixing, settings by `POKE_AMOUNT` (bursts per 100-tick window) and `POKE_BURST` (frames per burst). Data: `pow/target/pokesweep{,2}/sweep.csv`.

Round 2, 7 starts x 5 seeds (found / runs, all starts; king-bobomb fails without pokes too):

| setting | poked frames / 100 | found |
|---|---|---|
| no pokes | 0 | 26/35 |
| 2 x 2 | 4 | 5/35 |
| 3 x 2 | 6 | 4/35 |
| 5 x 2 (current) | 10 | 4/35 |

Round 1 (3 starts x 3 seeds) also had 8 x 2: 0/9, 10 x 1: 0/9, 3 x 4: 1/9, and the same picture.

- Pokes cost a weak search success, not length: runs that do reach the star are 1-47 frames longer than unpoked ones.
- Between 4 and 10 poked frames per 100 the success rate is the same within noise, so a lower rate buys little and gives up anti-replay margin (recorded routes survive 0/12 at any rate).
- A real miner absorbs it: the campaign miner (pow/miner/src/rrt) mined 11+ main-chain blocks at 5 x 2.

**Decision:** keep 5 bursts of 2 frames per 100 ticks, both streams mixed every frame.
