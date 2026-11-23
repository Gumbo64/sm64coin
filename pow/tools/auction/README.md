# The frame-length auction model

A chain-level Monte Carlo model of SM64Coin's difficulty rules
(`pow/sm64pow/src/rules.rs`). Each block is a race. The limit (the longest run
accepted, in frames) grows with wall time since the parent. Each miner's
best run so far (its *envelope*) falls as it searches. The block goes to
whoever's run fits the limit first, as the network sees it.

- `sim.py`: the model. It covers limit curves (today's grace + decay, or the auction's
  no-grace slope), the Monero retarget, miner envelopes (parametric profiles, or
  measured ones from `envbench` CSVs), clock skew and stamping ahead, the hold
  policy, propagation latency, power-on restarts (today's 3600 s gate or the
  restart delay), a withholding miner, and a capability frontier (miners can only
  do the first K points of a cycle: "intelligence softlocks").
- `sweep.py`: sweeps the slope (seconds per difficulty halving) across scenarios and writes
  `results.json` and the table below.
- `restart.py`: compares restart rules against frontiers, for fast bots and for today's slow
  bots, and writes `restart_results.json`.

```
python3 sweep.py --blocks 1200 --seeds 3 --workers 4 > sweep_param.md
python3 restart.py 30 60 > restart.md
```

The miner profiles in `sim.py` are assumptions (`DEFAULT_MINERS`, `SLOW_MINERS`), not
measurements. They are meant to show the shape of each effect.

## Findings (2026-11-22)

- **The slope decides what wins.**
  - A steep slope (30–60 s per halving) rewards the shortest run near the block time. The
    optimizer bot wins about 90% of blocks, and block times are tight: CV 0.3–0.5, p99 500–700 s.
  - A shallow slope (≥ 300 s) behaves like a fixed threshold: the first run in wins. The quick
    bot dominates, and the tails are long.
  - Today's rules (600 s flat grace) sit in the shallow regime: CV 1.7, p99 about 2,500 s.
- **Stamping ahead needs the hold policy.**
  - One miner stamping 120 s ahead while the others stamp honestly wins 97% of blocks at a 60 s
    slope.
  - With the hold policy (nodes accept a run only once it fits their own clock), the
    advantage is zero.
  - If every miner stamps ahead equally, the effect is symmetric and harmless.
- **Withholding** (holding a found block back to start on the next one) does not pay at steep
  slopes. The second-best run fits the limit soon after, so the withholder ends up in races and
  drops from 90% to 56% of blocks. Under today's flat grace it pays, rising from 42% to 55%.
  The withholder modelled here is blunt; a withholder that estimates the competition is an open
  question.
- **Restarts** are an escape from intelligence softlocks.
  - Pricing them in frames costs nothing once the limit is at its maximum (difficulty 1, which
    is where today's slow bots sit). A restart's first star is found in seconds, so restarts
    beat any star that takes longer than that, and the chain loops on easy stars.
  - So a restart is priced in seconds: it is judged against the limit as it stood `delay`
    seconds earlier.
  - With today's slow bots (tens of minutes per star), a short delay (1200 s) causes many
    needless restarts (restarts that beat a feasible star).
  - 3600 s + 60 s per point thrown away (cap 7200 s) keeps needless restarts at today's level
    (0–3 per 700 blocks), and bounds the time one star can hold the chain up.
- **Retarget:** the Monero retarget with constant weight converges, with block times at about
  the target at slopes of 30–120 s. It undershoots slightly at 15 s, because block time is about
  linear in log(difficulty).

The rules use a 60 s slope, the hold policy, the 3600 + 60 s/pt restart delay
(cap 7200 s), and fork choice by block count. The model has no deep forks, so
fork-choice weight doesn't enter it. A red-team pass showed
that weighing by difficulty lets a private chain stamped at its runs' earliest valid
seconds pump its weight (a time warp), so consensus counts blocks.
