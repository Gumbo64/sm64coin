# Head start and selfish mining in SM64Coin's time-limit auction: simulation results

Everything here comes from `hs.py` (simulator), `run_experiments.py` (all runs, fixed seeds), `analyze.py`
(tables.md and the PNGs), `solo_rate.py`. Raw per-run stats are in `results_*.json`; every table in
`tables.md` has the full detail (this file quotes the important rows). CIs are 95% t-intervals over 6 independent
seeds of 3000 blocks each (so about +-0.5 to +-1.5 pp on a block share). 4 worker processes.

## 0. Answer in short

1. **The head start is real and large in time** (the winner of a block has held its run for W = 130 to 210 s on average, about
   0.45 to 0.7 of the 300 s block time, and 80-100% of winning blocks have W > 1 s once the network has more than
   ~1 machine of compute). But **whether it turns into extra block share depends on how a miner can use it**, and in this model that is weak:
   - A miner that commits to its held run only when it is within H = 120 s of valid and then builds on it privately gets
     the #1 miner (40% compute) **+0 to +1.5 pp** (CI about +-0.7..1.2) and a 60% miner **+0 to +3.7 pp**, rising with competition.
   - The effect needs *everyone* to commit very early (H = 300 s) to become big: 40% -> 47.7% (+7.7 pp), 60% -> 74.9% (+14.9 pp), and the
     previous winner then wins 70-81% of the next blocks. **But a miner who does that alone loses** (share/compute 0.57-0.7 at C >= 30), because
     it stops improving the held run. So the rich-get-richer equilibrium is mild in this model (+1-4%, relative +3.7% for the 40% miner at best
     at H = 120, up to ~+6% for 60%). The effect scales with W/B and is zero when miners are symmetric (25x4, 10x10).
   - The memo's formula s = alpha/(1-delta(1-alpha)) is not normalised (it assumes p- = alpha, but p- < alpha); a two-state
     Markov chain with the measured p+ and p- matches the simulation exactly (section 3.4).
2. **The bigger problem is withholding, not the head start.** The auction makes block time almost independent of compute
   (a 10% miner alone extends a chain only ~25% slower than the whole network, `solo_rate.md`), and fork choice counts blocks.
   Combined with the head start, a withholding miner keeps a private chain that keeps pace:
   - with today's rules (H = 120 for all), selfish mining beats honest above **alpha = 0.30-0.35 even at gamma = 0**, and at gamma >= 0.5
     (a large miner with many well-connected nodes) at **every alpha tried (0.1: 19.6% vs 10% at C = 10; 25% vs 10% at C = 100)**; at gamma = 1 it takes 44-99% of blocks.
   - without the head start (H = 0) it is the classic game: no profit at gamma = 0, 0.5, small gain at gamma = 1.
   - a rule where the next search may start only at publication (`AtPublish`) removes this entirely (selfish = honest or worse).
3. **Fix variants** (section 5): the *race regime* (valid on find: shallow slope or flat) removes W and the excess, and the quality loss is
   about zero in this model (the best run saturates at a floor); slowing the chain only dilutes; valid-on-find + displacement window removes
   the head start but leaves the classic selfish threshold (1/3 at gamma = 0, 1/4 at 0.5, 0 at 1); uncles over-correct (big negative excess for the
   #1 miner); the freshness bound caps back-dating at about its bound but a naive back-dater under the bound gains share through private mining.

## 1. Model and assumptions (read these first)

**Rules** (mirrors `pow/sm64pow/src/rules.rs`, `chain.rs`, and the Monero fork; all read from code):
- Limit: `L(t) = 36000 * 0.98^(level8(d) + 8*start_halvings - 8*t/dh)`, floored at 600, `start_halvings = 20`, `dh = 60 s`
  (one halving per 60 s). A run of F frames is valid at `t_v = T_parent + 7.5 s * (level8(d) + 160 - ln(36000/F)/0.0202)` (clamped at 0), i.e. shortest first.
- Retarget: Monero's, window 60, cut 6, target 300 s (cumulative difficulty and timestamps along the chain).
- Seed excludes the timestamp but includes `prev_id` (the parent's block id) and the coinbase; so a miner needs the parent hash and end state: the finder
  knows them at find time, others at publication. Stamp must be > parent stamp; nodes hold a block until it fits by their own clock, so a held run can only
  be published at `t_v` and is stamped `t_v`.
- Fork choice: `weigh_by_blocks`: more blocks wins; ties keep the first-seen chain (`bei.height > current height` strict).

**Run discovery (assumption, not measured).** One compute unit = one web-miner machine: a finished search ("dot") every 60 s on average (Poisson; a miner with share
a of total compute C has rate a*C/60 per s). A dot's length is `1800 + LogNormal(median 6000, sigma 0.8)` frames, i.i.d.; a miner's best run (envelope) falls as
min of i.i.d. draws. Check: after 18-45 machine-minutes the median best is about 3,400 frames (measured web miner: 2,313-4,663). Sensitivity: `kappa = 0.3`
(dots get shorter the longer a miner searches the same tip) changes nothing material (section 3.5). The floor 1800 is a *saturating* envelope: **this
is the key model dependence** (it makes block time insensitive to compute); a spread-out distribution (sigma 1.2-1.5, floor 1000) gives the same conclusion for `solo_rate.md`.
C ranges 0.05 ("quiet", block time ~1,360 s at d = 1) to 300 ("busy").

**Miner behaviour.** Honest-rational miners: after a miner has a held run whose remaining wait is <= H seconds it *commits*: stops searching this height and starts
the next one on its own (unpublished) block, as the rule allows (`AtFind`). H = 0 is "no head start" (start at publication). If another block at the same height
becomes valid first, the miner abandons its private work. The head-start family is therefore {H}; **a miner that splits compute between the held height and
the next is not modelled**, so exploitation here is a lower bound. Propagation: 2 s fixed (3 s extra exponential in the freshness runs).
Ties: first seen; a competing equal-height block does not displace (own unpublished block is dropped).
Selfish miner: Eyal-Sirer SM1 (lead 0 adopt, 1 publish -> race, 2 publish all, >2 publish one); gamma = fraction of honest miners that switch to the selfish
block in a tie (a parameter, as the fork rule gives no gamma; first-seen means gamma is set by propagation).
Reward: 1 per block (Monero tail emission), other `Reward` hooks exist.

**Simulator hooks** (for the fix agent): `StartRule` (`AtFind`, `AtPublish`, `AtPublishDelay(D)`), `ForkChoice` (`ByBlocks`, `ByCumDifficulty`, tie rules
`tiebreak = first | shortest` + `omega`), `Reward` (`PerBlock`, `PerDifficulty`, `UncleReward`), `valid = auction | flat`, `dh`, `start_halv`, `target`,
`announce_early`, `backdate`, `fresh` (freshness bound), `selfish`, `gamma`, `H` per miner, `delay_jitter`.

## 2. Baseline grid: block share vs compute share

Full tables per scenario in `tables.md` (section "Grid ..."). Top miner's excess in percentage points (block share minus compute share), mean +- CI:

| scenario (top miner) | C | H=0 | H=60 | H=120 | H=300 | prev-winner wins % at H=300 (expected if no effect) |
|---|---|---|---|---|---|---|
| 40/30/20/10 (40%) | 3 | +0.10+-0.72 | +0.38+-0.67 | -0.31+-1.22 | -0.14+-1.22 | 44.0 (29.7) |
| 40/30/20/10 | 30 | +0.17+-0.72 | +0.24+-1.58 | +0.12+-0.80 | **+5.46+-0.91** | 60.2 (32.9) |
| 40/30/20/10 | 300 | +0.20+-0.80 | +0.06+-1.18 | +1.47+-0.70 | **+7.66+-2.02** | 70.3 (34.5) |
| 60/20/20 (60%) | 30 | +0.37+-0.92 | +0.69+-0.92 | +1.86+-0.54 | **+10.29+-1.19** | 72.8 (53.8) |
| 60/20/20 | 300 | +0.76+-0.38 | +1.32+-1.26 | +3.66+-0.68 | **+14.91+-1.36** | 81.0 (59.3) |
| 25x4 | 300 | -0.06+-0.78 | -0.14+-1.00 | -0.14+-1.02 | -1.11+-1.12 | 66.4 (25.1) |
| 10x10 | 300 | ~0 | ~0 | ~0 | ~0 | |

- Mean W (s) of the winning block at H=0 / 120 / 300: C=3: 131 / 172 / 277; C=30: 136 / 197 / 345; C=300: 142 / 214 / 383. W>1 s in 83-100% of blocks at C >= 3
  (37% at C = 0.05, 73% at C = 0.3). Quiet networks have W > 0 less often but still sizable: the limit at d = 1 keeps blocks held until a few hundred seconds.
- **Excess scales with W/B and with the top miner's size**; it vanishes for symmetric miners (25x4, 10x10) although the previous winner still wins more often
  (streaks), because everyone gets the lead equally. With one big and many small (40+30x2, 40% + 30 x 2%) the excess is *negative* at H >= 120
  (-2.1+-0.6 at C=30, H=120; -5.6 at H=300, C=10): small miners lose little by committing early, the big miner loses its faster envelope improvement.
- Plots: `excess_vs_competition.png`, `excess_vs_H.png`, `W_vs_competition.png`.

## 3. More checks

### 3.1 Incentive: who gains from using the head start?
(`dev` group; "who" deviates to lead H, everyone else H = 0.) Share/compute of the deviator, C = 30: #1 (40%): H=60 0.997, 120 0.94, 300 0.71, 600 0.71; smallest (10%): 0.92 / 0.91 / 0.57 / 0.12.
C = 3: only H=60 and H=600 > 1 for #1 (1.02, 1.09) within noise. **Committing early alone does not pay** in this policy family.

### 3.2 Rule hook check
`AtPublish` (next search may start only at publication) with H = 120 gives the top miner -0.8..-1.9 pp (40/30/20/10) / -3..-5 pp (40+30x2): no lead at all,
and the early commit wastes work. H = 0 `AtFind` is equivalent to `AtPublish` up to the propagation delay (+0.1..+0.2, see grid).

### 3.3 Sensitivity to the dot model
kappa = 0.3 (dots improve with search depth): excess at H = 300 is +2.84+-1.13 (C=10), +5.28+-2.24 (C=30), +8.17+-1.37 (C=100), +10.06+-1.61 (C=300), vs +2.10 / 5.46 / 7.81 / 7.66 at kappa = 0.

### 3.4 The memo's formula
Table in `tables.md` ("Check of s = alpha/..."), #1 miner. e.g. 40/30/20/10, C = 30, H = 300: p+ = 71.2%, p- = 24.0%, delta = (p+-alpha)/(1-alpha) = 0.52, memo s = 58.1%,
two-state Markov s = p-/(1-p+ + p-) = 45.4%, measured 45.5%. The memo formula assumes the miner's win chance after someone else wins is still alpha; it is
lower (p- < alpha), because the lead is shared among miners and the probabilities must sum to 1 (for alpha = 0.5 vs 0.5 the formula gives s > 0.5 for both).

### 3.5 Block time depends weakly on compute
`solo_rate.md`: mean time to the next valid block for a miner holding fraction alpha of the compute *alone*, at the difficulty the whole network settles on:
C=30: alpha 1 / 0.5 / 0.25 / 0.1 / 0.05 / 0.02 = 302 / 325 / 350 / 387 / 432 / 498 s; C=300: 301 / 312 / 326 / 348 / 369 / 403 s. Under wider run-length spreads
(sigma 1.2, 1.5) it is the same shape (C=300: 10% miner 333-336 s). Cause: the limit moves 1 halving per 60 s, so compute only enters through the
logarithm of the best run. Consequence: a minority private chain advances at ~80-100% of the public rate.

## 4. Selfish mining (withholding past validity) under the actual fork rule

Setup: one selfish miner with share alpha, five equal honest miners; same H for all (0 or 120); block share on the main chain. Full table in `tables.md`
("Selfish miner"), plot `selfish.png`. Gains over honest (pp), C = 10 / 100:

| gamma | H | threshold alpha | gain at alpha 0.1 / 0.3 / 0.45 (C=10) | at C=100 |
|---|---|---|---|---|
| 0 | 0 | none up to 0.45 | -6.2 / -5.5 / -1.8 | -6.0 / -6.8 / -1.1 |
| 0.5 | 0 | none (0.4 at C=100, +1.0) | -1.6 / -1.5 / -0.2 | -2.1 / -2.8 / -0.1 |
| 1 | 0 | ~0.1 (tiny) | +0.7 / +1.3 / +2.2 | +1.0 / +1.2 / +2.0 |
| 0 | 120 | **0.30** (C=10), **0.35** (C=100) | -4.8 / +3.5 / +15.2 | -5.5 / -1.3 / +20.2 |
| 0.5 | 120 | **<= 0.1** | +9.1 / +28.0 / +34.1 | +15.5 / +42.9 / +47.5 |
| 1 | 120 | **<= 0.1** | +33.7 / +53.4 / +47.8 | +78.4 / +69.0 / +54.6 |
| any | `AtPublish` | none (selfish <= honest) | | |

- With H = 0 the result is the classic game (the threshold is above 0.45 at gamma <= 0.5 here, higher than Eyal-Sirer's 1/3 because honest block arrival has a 2 s delay and the private
  chain must also meet the limit); with the head start the selfish miner's private chain starts every round ahead *and* keeps pace (section 3.5): at gamma = 1 it wins 99.9% of blocks at alpha >= 0.3, C = 100.
- gamma is not a protocol constant: it is set by propagation/first-seen. A large miner with many connected nodes pushes gamma toward 1.
- Under `AtPublish` private building is impossible, so withholding only costs: (e.g. alpha 0.45, C = 100: 42.6% vs 46.0% honest).

## 5. Fix variants from FIXES.md

### 5.1 A: race regime (slope dial) and F (slower chain)
`A` group: seconds per halving dh in {30, 60, 120, 300, 1000, 3000, flat (valid on find)}, `start_halvings = 0`, Monero retarget, 40/30/20/10.
Rows at C = 30, H = 300 (the bad case), then W and quality at H = 120:

| dh | #1 excess pp (H=300) | W s (H=120) | P(W>1) | mean winning run F (frames) | block time | stale % |
|---|---|---|---|---|---|---|
| 30 | +7.80+-0.95 | 213 | 1.00 | 2,602 | 295 | 6.6 |
| 60 | +5.05+-1.78 | 196 | 0.99 | 2,574 | 299 | 3.8 |
| 120 | +2.91+-0.63 | 170 | 0.92 | 2,542 | 300 | 2.5 |
| 300 | +0.88+-0.96 | 130 | 0.73 | 2,503 | 301 | 1.4 |
| 1000 | +0.12+-1.47 | 79 | 0.39 | 2,471 | 301 | 1.1 |
| 3000 | +0.36+-0.89 | 40 | 0.17 | 2,463 | 303 | 0.8 |
| flat | +0.51+-1.12 | 0 | 0 | 2,484 | 311 | 0.7 |

- W falls with the slope (dh=300: 0.6x, 1000: 0.4x, 3000: 0.2x) and **the excess disappears at dh >= 300-1000** even at H = 300. At C = 300, H = 300: +13.7 (dh 30) .. +1.9 (300) .. +0.5 (1000).
- **Quality (mean winning run length) barely changes**: +-1-3% at C = 30, up to about +8% (3,404 vs 3,135 frames, steepest vs flat) at C = 3. The auction buys little here
  because the envelope saturates at the floor (model-dependent: if real runs keep improving with search, the loss would be larger).
- Steep slopes (dh <= 10) make the Monero retarget unstable in this model (difficulty runs to 2^26-2^60, stale rate 14-74%; at dh = 30 stale is already 6-28%), see `README.md`
  of the original simulator. The flat variant at C = 300 hits my difficulty clamp (2^17): block time 201 s, not 300 (a clamp artefact, not a rule property).
- Today's constants (start_halvings = 20, H = 120): excess +0.26 / +0.39 / +0.60 / +1.07 pp at C = 0.3 / 3 / 30 / 300 (CIs +-0.5..1.7), W 174-215 s.
- `A_dial.png`, `A_W.png`.

`F` (target B = 300 / 600 / 1200, today's slope): W scales with B (131 / 272 / 554 s at H = 0, C = 3), W/B stays about 0.45-0.55 at H <= 120, and the H = 300 excess shrinks:
C = 30: +3.82 / +2.08 / +1.01 pp, C = 300: +8.07 / +2.00 / +2.04. A slower chain only dilutes.

### 5.2 C: valid on find + displacement window
`C` group (honest miners, valid-on-find, flat cap): excess -0.4..+0.5 pp (all within CI), W = 0, stale/displaced 0.5-1.2%. The displacement window omega
(15-120 s; shortest-run within omega beats first-seen) makes **no difference for honest miners** (identical numbers for every omega): siblings only occur within propagation delay, which is
much shorter than omega. Block time at C = 300 is 201 s (difficulty clamp, as above).
**Selfish under C** (`Cselfish`, C = 30): first-seen gamma = 0: gain starts at alpha = 0.35 (36.9% vs 35.2%) (+7.6 pp at 0.4); gamma = 0.5: threshold between 0.25 and 0.3 (34.9% vs 30.5% at 0.3);
gamma = 1: 11.1% vs 9.6% at alpha = 0.1. Matches Eyal-Sirer (1/3, 1/4, 0). Tie rule "shortest run within omega" behaves like gamma ~ 0.5 (54.0% at 0.4, identical for omega 30 and 60; the tie is only ever decided within propagation time): **it is not a defence**; it gives a withholder with a shorter run a deterministic edge.
`C_selfish.png`.

### 5.3 E: reward smoothing
Uncles (each stale sibling valid within 120 s of the winner gets u, the winner pays u per uncle, max 2): in today's auction almost every block has uncles (held runs are publishable),
and the #1 miner's reward share minus compute share goes from +0.7..+1.9 pp (u = 0) to **-2.3 (C=3), -11.0 (C=30), -14.7 (C=300) at u = 0.5** and -4.5 / -19.8 / -27.1 at u = 0.875: strong over-correction toward equal
split by participation, not by compute. Under C (valid on find) there are no uncles and no change. Shares/pro-rata payout (analytic, `tables.md`): reward = b*block + (1-b)*pro-rata,
so the excess scales by b (H = 300, C = 30: +5.5 -> +2.7 (b=0.5) -> +0.55 (b=0.1)). Uncles do not remove the head start (W, win streaks unchanged).

### 5.4 Controls
- **S0' (everyone announces held runs at find, others build on them)**: excess falls to -0.1..+0.4 pp at H = 300 (vs +5.5 / +7.7 for S0 at C = 30 / 300): confirms the mechanism is the private information.
  Cost: wasted work when the speculative block is displaced by a run valid earlier.
- **Back-dating** (stamp T_P + g(F) instead of the validity time), today's rules: no measurable share gain (e.g. +0.0..+0.4 vs S0, within CI) because W > 0 almost always (found runs are
  valid only at validity, no room to back-date); at C = 0.3 about 20% of the back-dater's blocks are back-dated by 31 s on average. With a shallow slope (A300: dh = 300, start 0) most
  winners are valid on find, so back-dating is common (frac 0.5-0.86), by 85-314 s at C = 3 / 0.3, **+1.5 pp** for the #1 miner at C = 3 (+0.6..+1.9 elsewhere, small); if *everybody* back-dates (A300, C = 0.3) the chain's
  stamps run 600 s/block behind real time (gain column), a retarget time warp.
- **Freshness bound** (`fresh` group; Delta = 30/60/120 s; propagation 2 s + Exp(3 s); stale blocks are not adopted or relayed, so fresh beats stale regardless of arrival order):
  - False-stale rate of honest blocks: <= 0.02% at Delta = 30 s (0.000% at 60/120 s).
  - A back-dater that stamps right at the bound (`aware`: never older than Delta - 10 s) gets a child-clock head start of about Delta - 10 s on its back-datable blocks
    (e.g. A300, C = 3: 9.1 / 21.7 / 41.8 s at Delta = 30 / 60 / 120 vs 85 s unbounded) and **+0.85 / +1.12 / +1.23 pp extra share** (CIs +-0.4..1.4) vs +1.51+-0.91 unbounded: the bound caps the gain at about Delta.
  - A *naive* full back-dater under the bound is dropped by honest nodes (blocks never adopted): in A300 at C = 0.3 it loses 32-38 pp of share (its blocks are always stale), but at C = 3 / 30 it
    gains +5.0 / +2.1 pp (Delta = 30) with a stale rate of 16% / 10%. Mechanism (not verified separately): its dropped blocks are invisible to the network while it keeps building on them privately,
    i.e. the bound opens a withholding channel for exactly the blocks it rejects; the honest chain keeps its first-seen tip until a fresh child of the private chain arrives.
  - Under today's rules (S0) none of this matters at C >= 3 (gain <= 3 s, no share effect).
  - The bound as stated in the proposal should therefore not drop stale blocks without also letting a longer chain override them (the stale test must apply to tips only).
  `fresh_bound.png`.

## 6. Conclusion

- **How bad is the head start itself?** In terms of *time* it is large (W about 0.45-0.7 of a block time at every competition level above ~1 machine). In terms of *block share* with
  rational partial use (H <= 120 s) it is **mild: +0..+1.5 pp for a 40% miner, +0..+3.7 pp for 60% (relative +4-6%)**, growing with competition and with the top miner's size; zero for symmetric miners.
  Strong effects (+8 pp for 40%, +15 pp for 60%, previous winner wins 70-80%) occur only when every miner commits >= 300 s ahead, which the deviation test says is not individually rational.
  Caveat: only commit-and-switch behaviour is modelled; a compute-splitting strategy would capture more.
- **The dangerous part is withholding**: with the head start and by-count fork choice, a withholder at alpha 0.1-0.3 can take 20-99% of blocks when gamma >= 0.5; at gamma = 0 the threshold is 0.30-0.35.
  The reason is structural: block time barely depends on compute, so a minority private chain keeps pace with the public one. This is a 51%-style failure mode at well below 51% for the *chain* (a private
  chain is as fast as the honest one), independent of reward unfairness. Not verified against real run-length data; it depends on the saturating envelope, but holds for wider spreads.
- **Ordering of fixes (this model):** (1) make the next search start at publication (`AtPublish`; needs the seed to depend on something only available after publication) removes both the head start
  and the selfish channel; (2) shallow slope / valid-on-find (dh >= 300-1000 or flat) removes W with ~0-8% run-length cost, leaving the classic selfish threshold (1/3 at gamma = 0, 1/4 at 0.5, 0 at 1 - a fork-choice tie rule does not help);
  (3) a slower chain only dilutes; (4) uncles/shares smooth reward, uncles over-correct; (5) freshness bound caps back-dating to about its width but needs the "longer chain overrides" clause.

## 7. Reproduce
```
cd pow/tools/auction/headstart
python3 run_experiments.py [grid dev kappa selfish A F C Cselfish E ctrl fresh]   # writes results_<group>.json (about 6 s/run, 4 workers)
python3 analyze.py       # needs numpy + matplotlib; writes tables.md and the PNGs
python3 solo_rate.py     # solo_rate.md
```
