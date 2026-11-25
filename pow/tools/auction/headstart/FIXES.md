# Fixing the head start: design memo and simulation variants

Status: research memo, 2026-11-25. Feeds the simulator on branch `headstart-research`.
Code read: `pow/sm64pow/src/{rules.rs,chain.rs,proof.rs}`, `web/src/lib/rules.js`,
`pow/tools/auction/{ENVELOPES,README,restart}.md`, and the Monero fork
(`sm64coin-release/monero`, `grep SM64COIN`).

## 1. The problem, restated against the code

What the consensus code does today:

- **Seed.** `Blockchain::sm64_seed` hashes the block's hashing blob with `timestamp = 0`. The blob
  holds `prev_id` and the merkle root (so the coinbase, hence the miner). The timestamp is not in
  the seed, so a miner can search without fixing the stamp. `prev_id` is the parent's *block id*,
  which commits to the parent's proof. A run for N+1 therefore needs H(N) and N's end state, and both
  are known only to N's finder until N is published.
- **Validity.** A run of F frames is valid iff `F <= max_frames(difficulty, T_block - T_parent)`.
  The stamp must be `> T_parent`, `<= now + 120 s` (`CRYPTONOTE_BLOCK_FUTURE_TIME_LIMIT`, was 2 h)
  and `>= median(last 60)`. `sm64_hold`/`chain::hold_until` holds a block until the *node's own clock*
  says the run fits, so stamping ahead gains nothing.
- **Fork choice.** `weigh_by_blocks`: more blocks win, ties keep the chain seen first. There is
  no tie rule on stamp, run length or hash.
- **Limit.** Grows from the parent's stamp: one difficulty halving per 60 s, starting 20 halvings
  below the retarget's, capped at 36000 frames.

So for a winner who finds run F at `t_f`, with `L(t_f) < F`, the block becomes valid at
`t_v = T_P + g(F)` (g = inverse of the limit curve). The head start is `W = t_v - t_f`.

**Two facts make W structural, not a choice:**

1. Even an honest miner cannot publish during W. Nodes hold the block until it fits. (It can
   broadcast it; nodes will hold it. Nothing makes other miners use a held block as a parent.)
2. The information that gives the lead is N's end state and hash. The game for N+1 starts from
   N's end state, and the seed commits to H(N).

**Lemma (what any fix must break).** The lead exists iff (i) validity is delayed past find time
(W > 0), and (ii) the search input for N+1 depends on content of N that only N's finder has.
Fixes fall into four classes:

- kill (i): valid-on-find, or a limit so generous the first find is already valid (race regime);
- kill (ii): make the N+1 search input independent of N;
- make the lead worthless: reward smoothing;
- force disclosure during W: unenforceable (section 3).

The value of the lead is roughly `(-dE/dt) * W`, where E is the miner's envelope
(best run found so far). It is largest when the envelope is still steep, that is early in a search
and at a binding limit.

**Observation: the problem only exists when the limit binds.** When the retarget sits at difficulty 1
(today: miners slower than target), the first run found is valid on find, W = 0 and nothing is
held. The head start appears as miners speed up and the retarget raises difficulty so the limit
starts binding.

**Side observation: back-dating.** Consensus accepts any stamp in `[max(T_P + 1, T_P + g(F)), now + 120]`.
A miner whose run was found *after* the limit passed it may stamp it as early as `T_P + g(F)`,
which is earlier than the real time. The child's limit clock then starts earlier than real time
(free extra limit for the child) and the retarget sees faster blocks. This is a separate
timestamp-slack issue. The simulator's "stamp = validity time" assumption hides it, and any
variant that treats stamps as evidence of time needs to account for it.

## 2. Literature, as it applies

| Topic | What it says | Relevance here |
|---|---|---|
| Bitcoin propagation advantage: Decker & Wattenhofer, [Information Propagation in the Bitcoin Network](https://cdecker.github.io/btcresearch/2013/decker2013propagation.html), IEEE P2P 2013 | Fork/orphan rate scales with propagation delay / block interval. | Our W is a private "propagation delay" of minutes, with B = 300 s. W/B ~ 1 instead of ~0.01-0.1. |
| Selfish mining: Eyal & Sirer, [Majority is not Enough](https://arxiv.org/abs/1311.0243), FC 2014 | Withholding blocks is profitable above a threshold alpha > (1-gamma)/(3-2gamma): 1/3 at gamma = 0, 1/4 at gamma = 1/2, 0 at gamma = 1. Uniform random tie-breaking gives gamma = 1/2. | Fork choice here is first-seen, so gamma is set by propagation. A big miner with many nodes pushes gamma toward 1 (threshold 0). |
| Optimal selfish mining: Sapirshtein, Sompolinsky & Zohar, [arXiv:1507.06183](https://arxiv.org/abs/1507.06183), FC 2016 | An MDP computes epsilon-optimal withholding policies. The threshold is strictly below SM1's; about 0.329 even when gamma = 0 is "tight". | The simulator can reuse their MDP as the "selfish miner" agent under each rule. |
| Heilman, [One Weird Trick to Stop Selfish Miners (Freshness Preferred)](https://eprint.iacr.org/2014/007/20140404:151131), FC 2015 | Prefer blocks with fresher unforgeable timestamps, which raises the threshold from 25% to about 32%. It needs a trusted timestamp source. | Our stamps are lower-bounded by the run (no earlier than `T_P + g(F)`) but not by find time. Not a freshness proof (see back-dating). |
| Zhang & Preneel, [Publish or Perish](https://eprint.iacr.org/2015/518), CT-RSA 2017 | Backward-compatible. Fork resolution by chain weight, where *published* intermediate (weak) blocks count. A withheld block loses to a published competitor. | Template for "reward early publication". Our weak blocks would be valid-but-not-yet-fitting runs. |
| Pass & Shi, FruitChains, PODC 2017 (ePrint 2016/916) | Fair reward sharing: rewards go to "fruits" (low-difficulty shares) in a window, so a block's find luck matters less. Approximately fair in the (1-delta) sense. | The right model for share-based payout (variant E). |
| Bissias & Levine, [Bobtail](https://arxiv.org/abs/1709.08750), NDSS 2020 | Average of the k lowest samples cuts variance and hampers selfish mining. | Our envelope is already an order statistic. A "mean of k best runs" block is a related idea, not studied here. |
| Szalachowski et al., [StrongChain](https://arxiv.org/abs/1905.09655), USENIX Sec 2019 | Weak solutions count towards chain weight and reward. | Same family as P&P and FruitChains. |
| Sompolinsky & Zohar GHOST (FC 2015; ePrint 2013/881); Ethereum uncle rewards | Count and pay stale siblings, so orphaning costs the honest less. | Variant D. |
| Ethereum proposer boost (PoS LMD-GHOST) | A timely block gets a temporary weight boost to stop late-block reorg games. | The analogue of a freshness rule; useful for tie/late-reveal design in variant C. |
| Chia ([signage and infusion points](https://docs.chia.net/signage-and-infusion-points/), [consensus challenges](https://docs.chia.net/consensus-challenges/)) | Challenges are only revealed when needed and cannot be influenced afterwards. 64 signage points per sub-slot, with a block infused a few signage points later, all driven by a public VDF clock. | Chia removes private information by making the *clock* public and the challenge a function of the clock, not of a finder's private block. We cannot: our N+1 search input is N's end state, which is inherently the finder's. |
| VDFs: Boneh, Bonneau, Bunz & Fisch, [ePrint 2018/601](https://eprint.iacr.org/2018/601), CRYPTO 2018; Pietrzak and Wesolowski constructions ([2018/712](https://eprint.iacr.org/2018/712)) | Sequential delay with fast verification. Applications: randomness beacons and leader election. | Variant B. |
| Time-lock puzzles (Rivest-Shamir-Wagner 1996); timed commitments (Boneh-Naor 2000) | Encrypt to the future. | Variant D. |
| Parallel chains (Fitzi, Gazi, Kiayias, Russell, 2018); Prism (Bagaria et al. 2019) | Decouple proposing and confirming. | Background for decoupling (variant G). |
| Proof of Elapsed Time (Intel Sawtooth); Avalanche | Trusted-hardware or subsampled sequential/latency consensus. | Neither suits a trustless PoW-like chain. PoET needs TEEs. |

(Entries without a link are cited from memory; the search tool did not return a URL.)

## 3. Candidate fixes

Notation: B = target block time (300 s), P the parent, `t_v` validity time, W head start,
`E_i(t)` miner i's envelope, `L(t)` the limit curve. "Monero change" means C++ outside `pow/sm64pow`.

### A. Race regime: make runs valid on find (steeper and higher limit, shorter or no auction) [(f)]

Mechanism: choose `start_halvings` and `decay_halving` (rules.rs parameters; `rules.js` mirrors
them) so the limit usually already exceeds the best run when it is found. W then collapses to about
zero. What is left is the ordinary Bitcoin propagation advantage, seconds against B = 300 s.

- **Removes the head start:** yes, if the limit is generous. Residual W is the part of the crossing that
  is limit-bound. A steep slope shortens it: `W ~ ln(F/L(t_f)) / slope`.
- **Cost:** it gives up the auction's *quality* selection. The winner is the first to find any run
  under L, not the shortest. This is the regime the chain is already in at difficulty 1. The parameter
  trade-off is "steep limit rewards the shortest run" (README) against "steep limit shortens W": the
  same knob, opposite effects. The simulator can map the frontier.
- **Monero change:** none. A rules/parameter change and a hard fork like any rules change.
- **Attack surface:** the existing set (stamp-ahead is handled by the hold policy). Selfish mining
  reduces to the classic Bitcoin game (threshold 25-33%).
- **UX:** shorter waits, nothing held.
- **Simulate:** parameter sweep. Define the metric `W_win = t_v - t_f` of the winner and the block-share
  excess `share_blocks / share_compute` per miner size.

### B. VDF on the parent hash [(b)] and external beacons [(a)]

Mechanism: `seed(N+1) = VDF_T(H(N))`, or a seed from an unpredictable beacon.

- **Removes the head start:** no. Validity time `t_v` is a deterministic function of the run and
  the parent stamp, so the holder knows it in advance. Anything the beacon derives from N is known to the
  finder at `t_f`; anything derived from other miners does not exist in [t_f, t_v] because there is no
  other block in that interval. The holder starts the VDF at `t_f`, others at `t_v`, so the lead is
  still W, just later. A VDF stretches *everyone's* time (dilution `W/(T + B)`), costs block time, and
  creates a sequential-hardware advantage (ASIC VDF), which is a new centralisation vector.
- **External beacons** (a later Bitcoin block hash, drand): an oracle dependency, grindable at the
  margin. Rejected.
- **Verdict:** dominated. Not worth simulating beyond "lead stays W, block time grows by T".

### C. Public wait: valid-on-find plus a displacement window [(c), (k)]

Mechanism: remove the private hold. A run is valid on find (`F <= cap(difficulty)`, a flat limit). The
auction becomes public: a competing sibling with a shorter run that is first seen within `omega` of the
first sibling displaces it. Honest miners broadcast at find and mine on the best tip. The wait `omega`
is the same for everyone and is spent in public, so there is no private information.

- **Removes the head start:** for honest play, yes (the lead becomes the propagation delay). What remains
  is a selfish-mining incentive to withhold (classic game, with `omega` like a bonus window) and
  displaced work: miners who built on a tip that is then displaced waste a block's worth of search.
- **Status quo comparison:** today the hold *forces* privacy even on honest miners. This change makes
  honesty *possible*, then leaves the classic threshold problem.
- **Cost:** needs a tie/displacement rule in fork choice. It is subjective (depends on arrival time), like
  first-seen, so near-ties can split nodes briefly. Needs Monero changes in `handle_alternative_block`
  and the `weigh_by_blocks` switch (a comparator instead of a bool), and the hold policy goes away.
- **Attack surface:** timestamp is no longer the clock (validity does not depend on it, retarget still
  reads it). Late-reveal reorgs are limited by `omega`. Avoid *lowest-stamp-wins* as the tie rule:
  a stamp cannot be earlier than the run allows, so a withheld earlier-stamped block would beat
  an honest competitor deterministically (effective gamma = 1, threshold 0).
- **UX:** blocks usable after `omega` (e.g. 30-60 s) with small displacement risk; no change for the web miner.
- **Simulate:** valid at `t_f`; search for N+1 starts at `t_f + prop` for the best-known tip;
  displacement if a shorter sibling arrives before `first_seen + omega` (waste the work done on the old
  tip); honest miners publish at `t_f`; the selfish agent uses the Sapirshtein MDP.

### D. Announce, commit-reveal, "valid when seen" incentives [(c), (d)]

Mechanism: require or reward publication of held runs during W: an on-chain commitment, or a rule that
the next block must reference announced held runs (the Publish-or-Perish idea), or time-lock-encrypt the run.

- **Removes the head start:** only if the finder publishes. A *commitment* does not help others search,
  since they need the end state, not a hash. Publishing the end state without the run is unverifiable.
  Time-lock encryption of the run (open after W) gives others the data only after W and needs a puzzle
  per block that every verifier can cheaply check was well formed (verifiable timed commitments).
- **Why it fails:** announcing is never strictly beneficial to the finder (it can only lose the lead),
  and there is no on-chain event in [t_f, t_v] to hang a penalty on. The only enforcement points are
  subjective gossip times.
- **Hybrid worth keeping:** publish-at-find as an *honest default* (the miner software already can),
  combined with variant C, which makes announcing meaningful.
- **Verdict:** not by itself. Cost is high, effect unenforceable.

### E. Reward smoothing: uncles, shares, PPLNS [(e)]

Mechanism options:

- **Uncle rewards (GHOST/Ethereum):** a valid run for the same parent that arrives within a window gets a
  fraction of the reward (cap 2 per block).
- **Share/fruit payouts (FruitChains, StrongChain):** runs valid under a looser limit (valid at find,
  W ~ 0) count as shares; the block reward is split over shares in a window.
- **Off-consensus pool:** a P2Pool-style sidechain for the same share concept, voluntary.

- **Removes the head start:** no. It lowers the *value* of winning: if everyone is paid on shares the
  block winner's lead translates into little. Uncle rewards pay losers who were valid within the window,
  which smooths luck but not the mean advantage of the lead.
- **Cost:** consensus complexity is high. Uncles need header fields, coinbase validation and uncle
  verification (Monero change, large). Consensus-enforced payout splits clash with Monero's one-time-key
  outputs (the verifier cannot check who a stealth output pays without the recipient address). A voluntary
  share pool needs no Monero change but needs adoption. The biggest miner has no incentive to join.
- **Attack surface:** share-withholding / pool-hopping, sybil shares (a share needs real compute, so no free
  lunch), uncle-mining incentives (publish deliberately losing runs).
- **Simulate:** reward function `R_i = sum over height h of (1-lambda*n_u) if winner else u if uncle`,
  where uncle = valid run (valid by `t_v^winner + omega_u`), `u` = 0.5-0.875 of base, `lambda*u` taken from
  the winner (keep supply constant). Shares variant: miners earn a share at each run valid under
  `rho * L(t)`, paid pro rata over the last K shares.

### F. Shorter W through block-time or limit shape [(f), part 2]

Mechanism: the lead share is `W / (W + B)`-like. Raising B (target) or shifting the auction so the median
crossing sits further past the typical find time both dilute it. This is the same knob as A, considered as
"slow the chain" rather than "make valid on find".

- **Removes the head start:** dilutes only. Cost is a slower chain (B up) and less game per unit time.
- **Monero change:** none (retarget target in rules.rs).
- **Simulate:** B sweep (300, 600, 1200 s) at fixed slope.

### G. Make the search input independent of N [(g), (a)]: lag-k seeds / canonical start states

Mechanism: block j plays from the end state of its ancestor j-k and is seeded from that ancestor's id (not
from `prev_id`). Then all miners can search for j from the moment j-k is public. Variant: start from
canonical states chosen per height (a library of savestates), which breaks continuity of the game across
blocks.

- **Removes the head start:** *not* by itself. The lead moves to block j+k, and pipelining makes it
  worse: the extra search time lets a run be found *before* the limit clock for its height even starts,
  so W grows by roughly (k-1)B. The lead fraction is about `W/(W + kB)`, which does not shrink as k grows
  (k = 1: W ~ 0.3B gives ~0.23; k = 2: W ~ 1.3B gives ~0.28 in a crude model). Canonical start states
  truly remove the dependence on any finder's data, but for a seed still derived from some block j-k the
  same leakage applies.
- **Cost:** Monero change in `sm64_parent_ref`/`sm64_seed` (ancestor lookup across alt chains); the game
  becomes k interleaved lineages; the state cache must hold k+ states. High effort, doubtful gain.
- **Simulate** (to confirm or refute): start state and seed from block `h-k`, clock from block `h-1`.
- **Verdict:** interesting, but the analysis says it moves rather than removes the lead. Low priority.

### H. Restrict the same coinbase winning twice in a row [(h)]

- **Removes the head start:** no, only from the one coinbase. One operator uses N keys. The seed binds a
  run to a miner key, not to a person, and Monero offers no way to link outputs. A consecutive-winner ban
  makes the big miner rotate keys (free) while small honest miners pay for nothing. Possible
  reduction in ranking: it is the one variant with no benefit, so skip.

### I. Variants of the limit clock [(c) as originally phrased]

- "Limit for N+1 grows from N's validity time": this is already so (`T_N >= t_v`; stamping later hurts the
  stamper). The lead is in search time, not in the clock.
- "N+1's run must be found after N is public": unenforceable (an offline search leaves no trace). Only
  *seed-independence* (G) or *valid-on-find* (A, C) change this.
- A fixed grace `omega` before the child's limit starts to grow: dilutes the lead (`W/(W+omega)`) at the
  price of block time, equivalent to F.

## 4. Ranking and recommendation

Ranking, on removing the lead first, then cost and robustness:

1. **A, race regime (valid on or near find), plus F as a dial.** Zero Monero code, rules.rs parameters
   only. Kills (i). The cost is the loss of the shortest-run selection, which can be traded off
   continuously via the slope. It is also the regime the chain is in at difficulty 1.
2. **C, valid-on-find plus a public displacement window.** Keeps some quality selection and makes
   honest publication possible. Moderate Monero change (fork choice comparator, remove the hold). Leaves a
   classic selfish-mining game that has known thresholds and known tie-rule mitigations (random tie-break,
   freshness/proposer-boost style).
3. **E, shares (FruitChains/StrongChain-like), then uncles.** Smooths the reward but does not remove the lead.
   Off-consensus P2Pool-style variant has no Monero cost. Large Monero cost on-chain.
4. **G, lag-k decoupling.** Moves the lead; may worsen it.
5. **D, announce/commit-reveal.** Unenforceable.
6. **B, VDF/beacons.** Dominated.
7. **H, consecutive-coinbase restriction.** Useless (sybil).

**Recommend:** A first, tuned by the simulator. If the frontier shows too much quality lost, add C: it is the
smallest Monero change that makes public waiting possible. Do not adopt a lowest-stamp tie rule.
Use E only as an optional second layer if block share still exceeds compute share after A or C.

## 5. Rule definitions for the simulator

Common model: height h, parent P published (to miner i) at `p_i`. Miner i's search for h
starts at `s_i`; its envelope gives runs `(F, t_f)`. Validity time `v(F)`. Reward `R`. Fork choice `FC`.
`prop` = propagation delay (set 1-10 s; gamma = fraction reached first by the attacker).

| Variant | Search for N+1 may start at | Valid at | Fork choice | Reward | Agents |
|---|---|---|---|---|---|
| S0 baseline (today) | finder: `t_f`; others: `t_v(N) + prop` | `max(t_f, T_P + g(F))`, stamp = validity | blocks, first seen | 100% winner | honest-hold; selfish withholds past validity |
| S0' announce-early (upper bound) | all: `t_f + prop` (speculative on held block; work wasted if displaced by a shorter run valid earlier) | as S0 | as S0 | as S0 | all honest |
| A race regime | `t_f + prop` | `t_f` if `F <= L_A(t_f)` else `T_P + g_A(F)`, with a steep/high `L_A` | first seen | 100% | sweep `start_halvings` in {20,15,10,5,0}, `decay_halving` in {60,30,10,5} s |
| F slow chain | as S0 | as S0 | as S0 | as S0 | sweep B in {300,600,1200} s |
| C public wait | `t_f + prop` (miners mine on best-known tip) | `t_f` if `F <= cap`; the tip may be displaced by a shorter sibling first seen before `first_seen + omega` (omega in {15,30,60} s), then work on the displaced tip is wasted | blocks, then shortest run within omega, then first seen (random tie-break as control) | 100% | honest-publish; selfish (Sapirshtein MDP) with gamma in {0, 0.5, 1}, and with `omega` |
| E1 uncles | as S0 or C | as S0 or C | as S0 or C | winner `1 - lambda*n_u*u`, each of up to 2 uncles `u` (runs valid within `omega_u = 60-120 s` after the winner's validity), u in {0.5, 0.875}, lambda = 1 | as base |
| E2 shares | as base | shares: valid under `rho*L(t)` (rho in {2,4}) at `t_f` | as base | block reward split pro rata over the last K shares (K = 20-100) and a winner bonus b in {0, 0.1} | as base; add share-withholding agent |
| G lag-k | from `t_v(h-k) + prop` (finder of h-k: `t_f(h-k)`) | clock from `T_{h-1}`, as S0 | as S0 | as S0 | k in {1,2,3}; metric `W_win`, share excess |
| H consecutive ban | as S0 | as S0 | as S0 | as S0 | big miner as 1 identity vs. split into 2-3 identities (expect no change) |

Metrics to report per run: (1) block share vs compute share per miner size (0.01-0.5);
(2) `W_win` distribution (`t_v - t_f` of winners); (3) conditional win probability
`P(win N+1 | won N)` minus compute share; (4) orphan/displaced-work rate; (5) block-time p50/p99;
(6) selfish revenue vs honest at alpha in {0.1,...,0.4}; (7) quality: mean winning run length.

Controls and checks:

- At alpha -> 0 every variant must give share = compute (sanity).
- Check S0 against the prediction `s ~ alpha / (1 - delta(1-alpha))` for the conditional advantage `delta`.
- Back-dating: run S0 and A once with stamp = `T_P + g(F)` (back-dated) instead of the validity time, to see the
  stamp-slack effect on child limits and the retarget.
- For C, keep the tie rule as a parameter: {first-seen, uniform random, shortest-run, lowest-stamp}. Lowest-stamp is
  expected to show gamma_eff = 1 for a withholder.

## 6. Open points

- The size of W in practice is the parallel agent's output. If the winner's `W_win` is small relative to B at
  the operating point (as the "second-best crossing soon after the first" remark in ENVELOPES.md suggests for steep
  slopes), A may be enough with just the slope.
- Whether quality selection (the shortest run) is a consensus *requirement* or a nice-to-have decides how
  far A can go.
- Variant C changes node behaviour (no hold), so it should be tested with the existing `hold_until` tests
  replaced by displacement tests.
