# Run clouds, envelopes and limits: notes on the graph

These notes cover the chart we use to reason about SM64Coin's difficulty (2026-11-22). They
record what we have learnt so far and where the same idea already exists in the literature.

## The chart

- **Axes.**
  - x: wall time since the parent block (equivalently, since the miner started searching).
  - y: run length in game frames (30 per second). Log scale, because difficulty moves in
    levels of 2% of length.
- **Dots (the run cloud).** One dot per completed search: one rolling-horizon GA (RHEA) or beam
  search from start to end. A dot is placed at the time the search finished and at its run's
  length, and is coloured by strategy or miner. Searches that gave up are ticks on the time
  axis. Real data: `pow/miner/src/bin/runcloud.rs` writes `pow/target/runcloud/*.csv`, and
  `runcloud_json.py` turns them into the page's JSON.
- **Envelope E(t).** The shortest run held by time t: the running minimum of the dots, drawn
  as a step line along the bottom of the cloud. Dots above it were found but already beaten.
- **Limit L(t).** The longest run the chain accepts at time t, with the valid region under it.
  - Today's rules: flat for a 600 s grace period, then slow growth.
  - The auction: grows from the parent's timestamp at one difficulty halving per 60 s.
- **Block.** Mined at the first t where E(t) ≤ L(t). Intersections of the two curves (and
  crossings between miners' own envelopes) decide who wins.

Pages: the first race explainer (https://claude.ai/artifact/AnBDPxdQcjXo2Lc4sEg357) and the
run-cloud page with real runs (https://claude.ai/artifact/MvyqNF1Lhuy5EFrZSKVD6j).

## What the geometry says (modelled in sim.py, sweep.py and restart.py)

- **The limit's shape decides what is rewarded.**
  - A flat or shallow limit is a threshold: the first dot under it wins, whatever its length.
    This rewards being quick to produce any run.
  - A steep limit sweeps quickly through the range of lengths, so the block lands where the
    envelope is at around the block time. This rewards the shortest run then.
  - In simulation, a steep slope (30–60 s per halving) gives tight block times: p99 500–700 s,
    against about 2,500 s today.
- **Difficulty shifts the limit and the slope tilts it.** The retarget moves the limit's
  starting level so the median crossing is near the target. When miners are slower than the
  target (today: about 640 s per Bob-omb Battlefield run on 6 loaded threads), the retarget sits
  at difficulty 1. The limit then starts at its 18,000-frame maximum, and the first run found
  wins, under either rule set.
- **Timestamps move the point, not the curves.** Stamping ahead slides the limit left by the
  future-time limit. One miner doing it while the others are honest won 97% of blocks at a 60 s
  slope. The hold policy (nodes judge "fits" by their own clock) removes the gain. A restamped
  copy keeps its y and moves along x.
- **The sequential head start** shows up as envelopes shifted right for everyone who heard of
  the parent later. Withholding is how a miner widens that shift. At steep slopes the
  second-best crossing comes soon after the first, so withholding mostly loses races.
- **A restart** is a separate cloud (power-on to an easy first star) judged against the limit
  as it stood `delay` seconds earlier. It caps how long one star can hold the chain up.
- **Pokes shape the cloud.** Stronger or less absorbable pokes push dots up and right. The
  sweet spot is where recovering from a poke needs search (the cloud stays spread out and
  intelligence moves the envelope) but precise routing still pays.

## The area under the envelope

- **Layer-cake identity.** Let t\*(ℓ) be the inverse of the envelope: the time until you hold a
  run of at most ℓ. Then

  ∫₀^∞ (min(E(t), b) − a)⁺ dt = ∫ₐᵇ t\*(ℓ) dℓ.

  So the area under the envelope, clipped to a band [a, b] of plausible limits, divided by the
  band width, is the **expected time to beat a flat limit drawn uniformly from the band**. It
  scores a miner across every limit without picking one, much as mAP averages precision over
  recall thresholds.
- **Use log frames.** Difficulty is uniform in levels, so the natural area is under log E(t),
  in difficulty levels × seconds.
- **The auction tilts it.** For limits c·gᵗ with an uncertain start c, the block lands where
  log E(t) − t·log g = log c. The relevant area is under F(t) = log E(t) − t·log g. A flat limit
  (g = 1) gives the plain area. A steep slope weights the early envelope.
- **Limits of the analogy.** It's an average over operating points. The retarget picks one
  operating point (near the target block time). Two miners whose envelopes cross can be ordered
  one way by area and the other way by who actually wins there. Win share needs the curves
  themselves, or an area weighted near the operating point.
- **Uses.**
  - A single score for comparing strategies and poke designs.
  - An objective for allocating search budget, if a learned model ever picks attempts.
  - Not for consensus.

## It already exists (mostly), in algorithm benchmarking

- **Convergence curves and anytime performance.** The envelope is a run's best-so-far
  convergence curve, one per strategy, here merged across restarts. In anytime optimisation,
  the area under it is the standard anytime-performance measure.
- **Target-based ECDF, COCO and IOHprofiler.** COCO fixes log-uniform targets (51 by default),
  records each run's time to reach each target, and minimises the area over the ECDF. That is
  our layer-cake identity with targets as the limits and log-uniform weighting.
- **Time-to-target plots and run-time distributions.** For a single limit, the time-to-target
  distribution (TTT-plot, or the run-time distribution of a Las Vegas algorithm) is the
  distribution of t\*(ℓ) across seeds.
- **The empirical attainment function (EAF).** It treats (time, quality) as two objectives. The
  dots are the attainment set; the envelope is the attainment curve. López-Ibáñez, Vermetten,
  Dreo and Doerr (2024) show that the area under the EAF equals the mean area over the runs'
  convergence curves, so targets need not be chosen in advance. There is an EAF module in
  IOHanalyzer and an EAF logger in IOHexperimenter.
- **Hypervolume.** Seen as a two-objective front (minimise time, minimise length), the area
  dominated by the envelope up to a reference point is the hypervolume indicator. That is the
  usual Pareto-compliant way to score anytime behaviour.

Sources:
- Using the Empirical Attainment Function for Analyzing Single-objective Black-box Optimization
  Algorithms (2024): https://arxiv.org/abs/2404.02031
- COCO: Performance Assessment: https://arxiv.org/abs/1605.03560
- Extensible Logging and Empirical Attainment Function for IOHexperimenter:
  https://arxiv.org/abs/2109.13773
- A model of anytime algorithm performance for bi-objective optimization:
  https://hal.archives-ouvertes.fr/hal-02898963v1
- Biobjective Performance Assessment with the COCO Platform: https://arxiv.org/abs/1605.01746

**What seems new here** is the second curve: a limit that is a function of time, tilted by
the slope and shifted by a retarget. Benchmarks use fixed targets. Ours are set by a
consensus rule, and a block is mined where the envelope and the limit meet. Who wins is
decided by intersections between curves, not by area alone.
