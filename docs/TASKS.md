# Open tasks

Work anyone can pick up (by hand, or by pointing an AI agent at it) without needing a decision from
the maintainer first. Each task has a clear goal and a way to check that it worked. Start with
[CORE.md](CORE.md) for how the project fits together.

**Not on this list, because they need a decision first:** anything that changes which blocks are
valid (consensus rules, `pow/sm64pow/src/rules.rs` parameters, proof format, fork choice), changes to
the Monero fork beyond `SM64COIN`-marked one-line hooks, and the wallet's key derivation
(`web/src/lib/wallet/passkey.js` is frozen). Open an issue for those instead.

## Ground rules

- **Consensus code must not change behaviour.** If you touch `pow/sm64pow` or `pow/statecodec`,
  re-verify every block on the chain before and after and show the results are byte-identical
  (verdicts, `proof_id`, decoded pads). The proof-id vectors (`web/wallet-wasm/test/run.sh`) must
  still match.
- **Measure, don't assume.** Search/miner changes need paired runs (same salts, both arms) with
  enough repetitions to beat the noise — single runs vary by 2× or more.
- **Small commits, moves separate from edits.** Say in the commit what you verified.
- **Never mine against mainnet from tests** unless the test is meant to submit; use a no-submit flag.

## Node and network

1. **Verification queue for many-IP invalid claims.** Today 60 bogus 36,000-frame claims from 20
   IPs still cost a node ~7 s of replay under the blockchain lock (see [DOS.md](DOS.md), "still open").
   Build a queue that replays the shortest claims first, outside the blockchain lock, with a per-peer
   share. *Done when:* the many-IP row in DOS.md's attack table drops to near zero and honest blocks
   still propagate in < 0.1 s. Touches Monero's block path, so keep the Monero side a thin hook.
2. **Per-IP budget refinements.** Peers behind one NAT share a budget, IPv6 addresses are cheap, and
   RPC behind a proxy sees one address. Group IPv6 by /64, trust a configured proxy's
   `X-Forwarded-For`, and test with the attack tool (`pow/tools/dos/attack.py`).
3. **Leaderboard scaling.** The leaderboard rescans the whole chain per entry; fine now, slow at
   ~10^5 blocks. Make it incremental (index by height), with a test that a rescan and the incremental
   result agree.
4. **`submit_block` honesty.** When the node's budget refuses a submission it can still answer OK.
   Return a distinct status the miners understand (and update the miners to show it).
5. **Pi performance.** Replays on a Raspberry Pi–class aarch64 board are ~15× slower than x86.
   Profile the emulator on aarch64 (perf), and try build flags / LTO / PGO. *Done when:* measured
   frames/s on aarch64 improves with byte-identical verdicts.

## Miners and search

6. **What should workers share?** Independent workers beat a shared tree (measured). Basic
   run-sharing restarts workers biased by the best run. Untested variants: insert the best run's path
   into each tree, attack different segments of the best run and splice improvements, keep half the
   workers unbiased. Compare them with the multi-worker harness: best run length over time after the
   first find, ≥4 paired salt sets on 2 targets, every shortened run replay-verified.
7. **Pipelined native executor.** The desktop miner's threads sit ~16% idle at the end of each search
   round. A pipelined executor (attempts in flight, results applied in a deterministic order) could
   recover that. Benchmark against rounds with `tools/seg120/bench.sh` in objective-rrt-v2
   (wall-clock time to the first star, ≥2 seeds per zone).
8. **Web miner speed.** Profile the WASM worker (emulator vs search vs feed), try SIMD and memory
   layout changes, tune the default worker count. *Done when:* frames/s per worker improves, measured
   on desktop and a throttled (4×) CPU.
9. **Seed grinding vs deep search.** For the same compute, do many shallow searches on different
   templates (different extra nonce) find shorter runs than one deep search? Measure; this informs
   miner defaults.

## Proofs and codecs

10. **Smaller proofs without a fork.** An encoder-side normaliser shrank proofs 10–13% with no
    consensus change (branch `codec-research`). Turn it into a production encoder option with the
    vectors still matching and every re-encoded chain block verifying.
11. **More codec research.** Better state-aware models for codecs 4/5 (decoder changes are consensus,
    so treat this as research: report size wins with a full chain re-encode, and propose).

## Save-state translator (sm64-binds)

12. **Fuzz coverage.** Extend `portable_fuzz` and the golden corpus to more movies, levels and
    cutscenes; every new divergence is a bug report with a minimal reproducer. See
    [PORTABLE.md](https://github.com/Gumbo64/sm64-binds/blob/main/docs/PORTABLE.md).

## Website and apps (low risk)

13. **Commit the test harnesses.** Many end-to-end scripts (browser mining runs, touch layout checks,
    chain re-verify harness, dependency-cycle check) were run from scratch directories. Bring them into
    the repo under `web/e2e/` and `pow/tools/` and run the cheap ones in CI.
14. **Accessibility.** Touch-controller buttons aren't keyboard-focusable and have terse labels;
    charts have no text alternative. Fix without changing the layout rules in the site's design.
15. **Explorer features.** Per-block run details (course, star, frames vs the limit at its stamp),
    replay links, cycle history.
16. **Group mining (iroh) hardening** (branch `group-mining`): a real end-to-end run shared between two
    machines, leader failover, the browser verifying received runs itself, and moving replay off the
    gossip task.

## Research (simulations, no rule changes)

17. **Auction simulator extensions.** `pow/tools/auction/headstart/` (branch `headstart-research`)
    simulates the time-limit auction with many miners. Useful additions: measured run-length-vs-compute
    curves, network latency models, and private-chain attack depth under different fork-choice rules.
    Results inform, but don't make, consensus decisions.
