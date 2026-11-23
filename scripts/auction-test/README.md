# Auction test on real nodes

`run.sh CHAIN_DATA WORK_DIR` mines three real runs on a chain's tip without submitting them, then
replays the frame-length auction on throwaway copies of that chain:

- **Test 1, one miner, same template:** the longer run is sent first and held; the shorter run
  from the same template is sent 3 s later. Expected node log: `holding block A`, `block B fits
  sooner than held block A ... replacing it`, `holding block B`, then `held block B accepted,
  relaying`; the tip is the shorter run.
- **Test 2, two miners:** the longer run (miner 1) is sent first, the shorter (miner 2) 3 s later;
  both are held. Expected: the shorter one is released first and becomes the block; the longer
  one never joins the chain; node B (connected only to node A) has the same tip.

Runs are sent with the website's own code (`web/src/lib/rules.js` for the valid time,
`web/src/lib/proof.js` to stamp and seal), bundled by `websubmit.mjs`.

**The tip must be fresh.** At the highest difficulty the limit still reaches its 20-minute cap
about an hour after a block, so on an old tip every run already fits and the first one seen wins
(no auction). Make a fresh tip first, e.g. mine one block on a test node, then copy its data dir.

The fixed difficulty for each test is chosen with `automine --pick-difficulty` so the runs become
valid about a minute or two from now; `automine --save-block` and `--salt` make the runs.
