# Denial of service: what a node replays, and for whom

Verifying a SM64Coin block means replaying its Super Mario 64 run on an
emulated N64, from its parent's end state. The run is up to 36,000 frames
(20 minutes of play), and state-coded proofs (codecs 3-5) are decoded during
the replay. That costs 0.05-1 s of CPU per block on a desktop core and about
15 times more on a Raspberry Pi. A proof-of-work coin checks a block with one
hash. So every path where a node replays a run is a lever for spam and denial
of service.

This document covers:

- the threat model;
- every path that leads to a replay, and who can trigger it;
- what was measured;
- the defences now in place (branch `dos-hardening`: `pow/sm64pow/src/admit.rs` and one-line hooks in the fork);
- what is left, and the consensus changes the owner has to decide.

## Threat model

The attacker has no special access. They can:

- open P2P connections from many addresses;
- call the public, restricted RPC (eboy.au's `16489`);
- read the chain;
- run the emulator.

They have no mining power beyond what they pay for. They want one of three things:

- burn the victim's CPU;
- delay or stop the victim from accepting honest blocks (stalls, forks, a node that falls behind);
- fill its memory or disk.

The victim is any node: a home node, the gateway, or the Pi.

What the attacker can make, and what it costs them:

| input | attacker cost | why |
|---|---|---|
| **valid variant** of any published block | about 0: a few emulator frames each | The run is malleable. Changing the proof's last byte (or dropping it) often gives another valid run: the last 1-13 inputs differ, the star still lands on the same frame. The variant has the same seed (the seed hashes the header and miner tx with the timestamp zeroed, and **not** the proof: checked, `get_block_hashing_blob` vs `get_block_id_blob`), the same length, and a new block id. Measured on mainnet blocks 1-34: **34 of 34** have a valid variant among the four cheapest edits (last byte xor 01/80/ff, drop last byte), 73 in all. For block 34, 23 of the 255 other values of the last byte give a valid run. For block 33 the first 103 edits tried gave 40 valid variants, and for block 20 the first 70 gave 40. Each variant is canonical: the codec's re-encode check (`replay_state`) passes, because it is a different run, not a different encoding of the same run. |
| **re-stamped copy** | free | Only the timestamp changes: new id, same run. Since red-team finding F1 it is not replayed again, but it was stored. |
| **invalid claim** | free | Change the nonce, so the seed changes and the pokes no longer fit the run. It is invalid, but only a replay to the claimed length shows it. |
| **fake sync span** | free | The blocks only have to parse and chain. |
| **valid block with a new template** | real mining | You need a run under that template's pokes. |

## Measurements

The machine is x86-64, one core per replay, measured while the box was shared (load average 30-50 on 16 cores). The numbers are from `cargo run --release --example dos_bench <rom> chain.jsonl [variants|sweep]` on mainnet blocks 1-34.

| what | cost |
|---|---|
| verify (replay) | 36-48 k frames/s, about 21-28 µs per frame. All chain blocks use codecs 4 and 5; both measure the same |
| the emulator alone, same pads | 56-68 k frames/s. Pokes, the state codec and the checks add 20-30 % |
| one chain block (1,393-9,256 frames) | 0.05-0.25 s |
| one invalid 36,000-frame claim (the longest limit) | 0.35-0.9 s |
| a claim of 2^20 frames (the codecs' cap) | about 25 s (only the sync pre-pass ever replayed this far; now it doesn't) |
| one valid variant as an alternative block, in the daemon (before) | about 0.58 s: replay, plus an xz write of its 8 MB end state, plus rebuilding the parent's state when variants had pushed it out |
| `proof_frames` (reading the claimed length) | 10 ns |
| Raspberry Pi (Rock Pi 4C+, RK3399, 2×A72 + 4×A53), **estimate** | about 15× slower (from commit 0fde528's measured 15 s on x86 vs 220 s on the Pi for the same cycle). That is about 2.5-3 k frames/s, 1-4 s per typical block, 12-15 s per 36,000-frame claim, and about 6 min per 2^20-frame claim. Not measured on the Pi itself (we don't touch it) |

**Natural bound.** A claim must fit the limit at its own timestamp, which is at most 120 s ahead of the clock. The limit grows from the parent's timestamp, so the most a claim can cost is `max_frames(d, now + 120 - parent_ts)`. Shortly after a block that is a few thousand frames; it only reaches 36,000 when the chain has been stuck for about 20 minutes. In the tests below the parent was hours old, so every claim could cost the maximum.

## Inventory: every path that replays

The **before** column is `main` at f790385 (fork c4ca7efe0). The **after** column is `dos-hardening`.

| # | path | trigger, who | before: checks done before paying for a replay | before: after an invalid replay | after (defence) | status |
|---|---|---|---|---|---|---|
| 1 | P2P `NOTIFY_NEW_FLUFFY_BLOCK` / `NOTIFY_NEW_BLOCK`, block extends the tip (`add_new_block`, then `sm64_hold`, then `handle_block_to_main_chain`) | any peer, unasked | parse; known id; timestamp in the future window; frames ≤ limit at its stamp (inside Rust, before the replay) | `bad_pow`: score 10, so the connection is dropped; a second time, 24 h IP ban. **A held claim that fails is replayed twice** (once in `sm64_hold`, again on the normal path) | per-source budget of claimed frames (`admit_relay`: 144 k frames burst, 36 k/min); a claim that fits no sooner than a held valid run on the same parent is not replayed; invalid runs cached by run (the second replay and resends are free); a full hold drops instead of "process now" | done |
| 2 | P2P, alternative block: sibling of the tip or deeper (`handle_alternative_block`) | any peer, unasked | parse; height; timestamp | as #1. **A valid variant is never punished**: it is replayed, stored in the alt-block DB, its 8 MB end state written to disk, and it pushes the tip's state out of memory and disk | `sm64_skip_alt` → `admit_alt`: at or below the tip, a variant or re-stamped copy of a block we have (same seed, from the policy's index or the main chain's block at that height) is skipped; new templates get 2 replays per parent; a sync span reaching above the tip is always admitted | done |
| 3 | P2P sync: `NOTIFY_RESPONSE_GET_OBJECTS`, then `prepare_handle_incoming_blocks`, then **preverify** (parallel), then #1/#2 per block | any peer claiming a longer chain (an unknown `top_id`) | parse; first block's parent is the tip. **No limit at all**: preverify replays every power-on block of the span to its claimed length (up to 2^20 frames), on every core, **holding the blockchain and txpool locks**, before any rule looks at it | the sequential check finds the first bad block, then `bad_pow` and a drop | preverify never replays past the longest limit any block can have (36,000), stops at the first invalid block in the batch (wasted work ≤ 1 replay per thread), and skips runs known invalid | done |
| 4 | held block becomes due (`on_idle`, then `sm64_release_held`) | internal | already verified: the known-run fast path, no replay | n/a | unchanged | ok |
| 5 | RPC `submit_block` | **allowed on restricted RPC**; anyone | parse; size; parent known (restricted). The rx-epoch check is meaningless here | **nothing**: no ban, no rate limit | per-address budget + a global restricted budget (`RPC_GLOBAL_FRAMES`, 720 k frames burst, 216 k/min); an invalid proof locks the address out (10 min, doubling, up to 24 h); also goes through #1/#2 | done |
| 6 | RPC `get_block` with `sm64_played` | restricted; anyone, any height | played-pads cache (8 M frames) | n/a | the RPC replay budget (`RpcScope`: 20 s of CPU burst, 12 s/min); RPC replays go to their own cache pool and can't evict the chain's states | done |
| 7 | RPC `get_sm64_cycle` (any block hash, alternative blocks included) | restricted; anyone | played cache | n/a | as #6 | done |
| 8 | RPC `get_block_template` with `prev_block` = an old block (`sm64_progress`, then `sm64_rebuild`) | restricted; anyone | none | n/a | as #6 | done |
| 9 | **rebuild** (`sm64_rebuild`): any check whose parent's end state isn't cached | via #2 (deep alternatives), #6-#8, and after evictions | none: walks back to a cached state or the cycle start (up to about 120 blocks) | | **End states split into two pools**: 16 for what the chain needed (verify, pre-pass, rebuilding a parent), 8 for replays an RPC call caused. A miss reloads the state from disk before rebuilding | done |
| 10 | `get_sm64_state`, `get_info` | restricted | cache only | | | ok, no replay |
| 11 | leaderboard (`NOTIFY_SM64_BOARD`, `sm64_board_join`) | anyone | signature; 30 scans a minute | | no replays. Each scan costs one key derivation per coinbase in the chain (O(height)) | ok now; revisit when the chain is long |
| 12 | txpool | anyone | stock Monero | | no replays | ok |

## The defences (non-consensus, implemented)

All decisions are in Rust. `pow/sm64pow/src/admit.rs` is new; `chain.rs`, `cache.rs` and `preverify.rs` changed. The daemon passes facts through one-line hooks marked `SM64COIN` (fork commit a24c92670 on branch `dos-hardening`):

- `cryptonote_protocol_handler.inl`: an RAII `sm64coin::source_scope` around the handling of a relayed block (the peer's host) and of a sync span (the host and the height the span reaches).
- `blockchain.cpp`, `handle_alternative_block`: `if (sm64_skip_alt(b, id, bei.height)) return false;`. `sm64_skip_alt` gathers the seed, the claimed frames, the heights and the main chain's seed at that height, and asks `admit_alt`. A skip sets no `bvc` flag: not stored, not relayed, the sender not punished.
- `blockchain.cpp`, `sm64_hold`: `admit_relay(frames)` (soft drop); the "a held run on the same parent fits as soon" check; a full hold drops the new block.
- `core_rpc_server.cpp`, in `submit_block`: on restricted RPC, `rpc_submit_allowed(host)` and a `source_scope(host, rpc_submit)`. In `get_block` / `get_sm64_cycle` / `get_block_template`: an RAII `sm64coin::rpc_scope`.

**Why skipping variants is safe for fork choice.** Fork choice counts blocks, and a tie keeps the block seen first. So a second block at a height we already have, variant or not, can't change our chain until something is built on it. When something is (an honest race, or an attacker who raced a variant to some miners), its descendants arrive. A relayed child is an orphan, so the node asks for the chain. The span then reaches above our tip, and `admit_alt` admits every block in it. New-template forks that span several sync batches pass through the per-parent allowance (each block has its own parent). A peer that lies about the rest of a span pays the way Monero makes it pay: the first invalid block gets `bad_pow`.

The policy functions have unit tests: `admit::tests` (variants, the per-parent cap, per-source budgets and refill, the RPC penalty and its doubling, the global RPC budget, the RPC replay budget, the bounded maps, nesting of the thread context) and `cache::tests` (RPC replays don't evict the tip). There are also integration tests on real mainnet blocks, `tests/dos.rs`:

- a malleated variant is valid, has a new id, and the policy skips it unless a span reaches above the tip;
- 16 fake 2^20-frame claims cost the pre-pass 0 replays, and 32 invalid 36,000-frame ones cost ≤ 4;
- invalid runs are remembered, while a rule that depends on the timestamp is not;
- RPC replays get `busy` once the budget is spent, while cached answers and consensus replays go on.

The whole consensus suite passes. All 34 mainnet blocks still verify with the same verdicts, and a fresh node syncs the 35-block chain from a `dos-hardening` node in 5 s.

## Adversarial test: before vs after

**Setup.** The victim is a monerod copy of the mainnet data (35 blocks), with block 34 popped, so its tip is 33. It runs on 127.0.0.1 with `--max-concurrency 4`, `--sm64-solo`, and `--add-exclusive-node` pointing at a dead port, so it never reaches the real network.

**Attacker.** `pow/tools/dos/attack.py` is a minimal levin peer: it does the handshake, sends `NOTIFY_NEW_FLUFFY_BLOCK`, and serves `RESPONSE_CHAIN_ENTRY` / `GET_OBJECTS`. Each connection comes from its own 127.0.x.y, so Monero's one-connection-per-IP limit and IP bans apply as they would to real peers. The blocks come from `pow/sm64pow/examples/dos_blocks.rs`, built from the real blocks: the ids and seeds of all 34 blocks are rebuilt from their parts as a self-test.

**Honest block.** The real block 34 (2,874 frames) is relayed by another peer 1-15 s into each attack. We measure how long until the victim's tip includes it. With no attack it takes 0.05 s.

**Measures.** Victim CPU is utime+stime of the monerod process over the run.

| attack | blocks | before: victim CPU | before: honest block | after: victim CPU | after: honest block |
|---|---|---|---|---|---|
| (a) 40 valid variants of block 33 (the tip's siblings), 4 IPs | 40 | 23.3 s; 40 alt blocks stored; the 8 end states on disk replaced by variants (a restart would rebuild the cycle) | 1.04 s | **0.02 s**, 40 skipped | 0.06 s |
| (a') 40 valid variants of block 20 (deep: a rebuild of 19 blocks each time the cache is pushed) | 40 | 31.1 s; 40 alt blocks stored | 1.71 s | **0.04 s**, 40 skipped | 0.01 s |
| (a'') 40 re-stamped copies of block 33 | 40 | 0.02 s (F1 fast path), but 40 alt blocks stored | 0.06 s | 0.02 s, 40 skipped, nothing stored | 0.06 s |
| (b) 60 invalid 36,000-frame claims on the tip, 20 IPs | 60 | 7.3 s (20 replays: each IP's first invalid claim drops it) | 6.99 s | 7.4 s (unchanged: see below) | 6.88 s |
| (c) fake sync span: 4 chained power-on blocks claiming 2^20 frames, 1 peer | 4 | **74.6 s on 4 cores**, node locks held | **24.9 s** | **0.01 s** (0 replays) | 0.05 s |
| (d) 60 invalid claims via restricted `submit_block`, 4 threads | 60 | **52.5 s** (60 replays, no penalty ever) | 5.2 s | **0.7 s** (1 replay, then the address is refused) | 0.06 s |
| (e) restricted `get_block sm64_played` walking the old cycle (blocks 1-24), 3 threads, 20 s | 17 k calls | 19.3 s, mostly serving JSON; the tip's end state evicted, so the honest block waited for a rebuild of the cycle | 0.88 s (9-block cycle; ~120 blocks on a long one, minutes on a Pi) | 18-21 s serving JSON (stock RPC cost, cached answers) | **0.01 s** |

Extrapolations for the "before" numbers:

- A real span can be up to 100 blocks (the victim asked for "4 / 100"). Before, one fake 100-block span would have cost about 100 × 25 s / 4 threads ≈ 10 minutes of a stalled node per peer, and about 6 min × 100 / 6 cores on a Pi.
- Valid variants are unlimited and never punished. Before, an attacker sending one per second kept a node at ~60 % of a core forever, grew its alt-block DB, and kept its disk states useless.

Reproduce:

```sh
cargo build --release -p sm64pow --example dos_blocks --example dos_bench
dos_blocks <rom> chain.jsonl variants 33 40 > var33.jsonl        # also: restamps, invalid, fakechain
attack.py fluffy 127.0.0.1:P2P var33.jsonl --ips 4 --rpc http://127.0.0.1:RPC/json_rpc --pid PID --honest honest.jsonl
```

`chain.jsonl` is one `get_block` per line, with id, prev, ts, nonce, proof, blob and miner_tx_hash.

## The replay queue (v0.3: "What is left" 1 and 2, done)

Blocks that arrive one at a time (a relayed block, a fluffy block, `submit_block`)
are no longer replayed on the thread that received them, under the blockchain
lock. Sync batches still use the parallel pre-pass. The code is
`pow/sm64pow/src/queue.rs`; the Monero side is `Blockchain::sm64_hold` /
`sm64_hold_alt` (queue a claim), `core::sm64_release` (add and relay when the
queue has a verdict), and one worker-side callback, `Blockchain::sm64_chain_facts`.

1. **Arrival** (under the lock, about 0.2 ms, no replay): parse; signature
   (`sign.rs`); the one-run-per-key rule (a second different run of a template
   is evidence, not replayed, not relayed); the per-source frame budget
   (`admit_relay`, unchanged).
2. **Queue by due time** (`hold_until`: the run must fit the limit by this
   node's clock; stamps more than 300 s ahead are rejected, and restricted RPC
   refuses a run due more than 30 s after it arrives). The queue holds at most
   256 claims, 16 per source; when it (or the source's share) is full, the
   claim due last makes room for one due sooner, so far-future claims can't
   keep a due claim out. A source whose claim replayed invalid is refused for
   10 minutes, doubling, and its other waiting claims are dropped. The
   source's replay budget (admit.rs) is checked on arrival and **charged only
   when a replay runs**. A loopback restricted-RPC address (the public
   gateway reaches the node through cloudflared on 127.0.0.1, so every web
   miner has that address) gets no per-address penalty, budget or queue
   share: one bad client can't lock the others out; it is bounded by
   restricted RPC's global budget. A refused claim is a `submit_block`
   error, not OK.
3. **Due: skip unless it could matter.** The worker asks the daemon what is at
   that height. A claim extending the tip is replayed. A sibling of our block
   there is replayed only if it has strictly fewer frames (the signed claimed
   count) and is inside the displacement window (`pof::candidate`), otherwise it
   is dropped without a replay. So once an honest block exists, every longer
   fake costs nothing.
4. **Replay on a worker pool** (`--sm64-replay-threads`, default every core),
   outside every lock, shortest claim first with a per-source fair share: a
   claim's priority is its frames plus the frames already replayed for its
   source (half-life 60 s). With two or more workers, **one is reserved for
   claims of at most 9,000 frames**, so an honest claim waits at most one
   short replay, never a pool full of 36,000-frame ones. Candidates for one
   height share the parent's end state and are independent.
5. **Release.** A releaser thread takes the verdict: invalid or skipped
   claims are dropped; a valid one is added (the check under the lock finds
   the cached run, 0.1-0.2 ms, no replay) and **relayed only then**.
   Consequences: a held run is not relayed before it is due and replayed; and
   a peer that sent an invalid claim is no longer dropped and banned by the
   P2P layer (the replay is asynchronous), it is refused by the queue
   instead. Its budget is spent either way.
6. A claim the queue cannot decide (parent state not cached) falls back to the
   old path under the lock (`sm64_bypass_scope`).

The local miner's `submit_block` (unrestricted RPC) replays on the RPC thread
before taking the lock (`sm64_prereplay`) when it is already due, so it stays
synchronous; one submitted before it is due is queued like any claim (it used
to be added up to the submit lead early). Unasked siblings of the tip that
can't displace it are dropped on arrival (they used to get admit.rs's
per-parent quota of replays under the lock). The parallel pre-pass runs only
for sync spans, and skips a run longer than difficulty 1 allows at its gap to
the parent.

### Measured: before and after the queue (x86, v0.3 test chain)

Setup as the earlier test, on the v0.3 chain: a victim `monerod` on 127.0.0.1 holding only the genesis
block (so every claim is a 36,000-frame replay from power-on), `--max-concurrency 4`, `--sm64-solo`, an
exclusive peer on a dead port; the queue run with `--sm64-replay-threads 4`. The claims are signed with
throwaway keys against the victim's own templates and carry junk inputs of the claimed length
(`pow/miner/examples/dos_claims.rs`; they replay to the end and then fail). Each source IP is its own
127.0.x.y peer (`attack.py fluffy --ips N`). The honest block (3,957-4,277 frames, a real mined run)
is relayed by another peer 3 s into the attack. "Before" is `hardfork-v0.3` at 3bbb414dc, "after" the
queue. The machine was shared (a 4K render was running), so latencies carry noise; CPU is utime+stime
of the victim.

| attack | before: victim CPU | before: honest block accepted after | after: victim CPU | after: honest block accepted after |
|---|---|---|---|---|
| no attack | 0.35 s | 0.10 s | 0.36 s | 0.05 s |
| 60 long fakes (36,000 frames, 60 keys), 20 IPs | 12.2 s | **8.94 s** | 13.4 s | **0.16 s** |
| 200 short fakes (300 frames), 20 IPs | 0.55 s | 0.11 s | 0.61 s | 0.05 s |
| 100 same-length fakes (4,000 frames), 40 IPs | 3.2 s | 0.11 s | 7.7 s | 0.05 s |
| 100 long fakes, 100 IPs | 36.2 s | **never** (the handshake timed out after 30 s; 39 of 100 connections dropped) | 13.3 s | **0.10 s** |
| 50 different runs of one key's template (3,000 frames), 4 IPs | 0.57 s | 0.10 s | 0.41 s | 0.05 s |
| honest block first, then 60 long fakes, 20 IPs | 2.2 s | 1.25 s | 4.2 s | 0.68 s |

Reading it:

- What the queue buys is **honest-block latency under attack**: 8.9 s to 0.16 s for 60 long fakes, and
  "never" to 0.1 s for the many-IP flood. The replays used to run one after another on the P2P threads
  under the blockchain lock; now they run on 4 workers outside it, and the honest claim is shorter than
  every fake, so it goes first.
- It does **not** reduce total CPU when the fakes arrive before any honest block: every claim extends the
  tip, so it could matter and is replayed (the numbers above are about equal, up to noise, or higher: 7.7
  against 3.2 s for the same-length burst, because the old path drops each peer on its first invalid
  block, so most of the burst was never replayed, while the queue has no connection to drop; the penalty
  now falls on the source's later claims). The skip rule pays once an honest block exists: in the last row
  the fakes after it are not replayed (CPU 4.2 s, of which part is the fakes that raced the honest
  block's due time, against 60 x 0.2 s).
- A worker pool is not preemptive: an honest claim that arrives while all workers are busy with
  36,000-frame replays waits for one of them (0.68 s above, a 36,000-frame replay is about 0.2-0.6 s here).
  On a Raspberry Pi (about 15 times slower) that wait is up to several seconds; reserving a worker for
  short claims would bound it, and is not done.
- The one-key rows (50 runs of one template) cost 0.41 s: the first run is replayed, the other 49 are
  evidence checks.
- **Raspberry Pi: not measured.** The aarch64 cross-build and a test node on the Pi did not fit the
  release deadline.

Reproduce: `net/scen.sh`-style: start the victim, `dos_claims RPC fake N FRAMES [KEYS] > claims.jsonl`,
`dos_claims RPC honest SAVED.json > honest.jsonl`, `attack.py fluffy P2P claims.jsonl --ips N --rpc
URL --pid PID --honest honest.jsonl`.

## The DoS audit of v0.3 (2026-11-25), fixed

Measured on local nodes by the audit; each fix is in pow/sm64pow (decision) with a thin hook.

- **N1** An unasked block on a parent deeper than the tip's parent made the daemon rebuild that
  parent's state (a replay of its cycle, 24 blocks: 2.5 s CPU, `get_info` blocked 2.8 s; a Pi
  about 40 s) under the blockchain lock, for one `submit_block`. Now such a block (relayed or
  restricted RPC) is not replayed at all (`admit_alt`: a longer fork comes as a sync span), and
  `chain::check` rejects a run over the limit or a known-invalid run *before* it needs the
  parent's state. Not done: a rebuild triggered by a sync span still runs under the lock (a
  sync span is asked for, and the peer is dropped on the first invalid block).
- **N2** `get_sm64_state` (6.7 MB per request) and `get_sm64_cycle` on restricted RPC share
  one byte budget (128 MB burst, 64 MB a minute: `admit::RPC_BULK_BYTES`); over it, "Busy".
- **N4** Restricted `submit_block` no longer re-encodes the proof (xz -9e before any check
  was 0.12 s of free CPU per 36,000-frame proof).
- **N5** `get_sm64_cycle` with an unknown hash no longer logs a stack trace; the equivocated-tip
  warning is logged at most once a minute.
- **N6** The xz decoder has a 128 MiB memory limit (a crafted header asked for 4 GiB; honest
  preset-9 streams need about 65 MiB).
- **N7** `--sm64-replay-threads` defaults to every core but one (P2P and RPC keep a core).
- **N9** The evidence file is written at most every 10 s.
- **C4/C6** Miners cap the inflated parent state at 32 MiB; automine no longer panics on short
  strings from the daemon.
- **The shared gateway.** Every web miner reaches the public node through cloudflared on
  127.0.0.1: a loopback restricted-RPC address gets no per-address penalty, budget or queue
  share beyond half the queue; restricted RPC's global budgets bound it.
- **Queue reservations.** A claim's frames are reserved from its source's budget on arrival and
  refunded if it is never replayed (refused, evicted, purged, skipped), so a burst can't queue
  more than the budget. When the queue is full, the worst-ranked claim (not due within 2 minutes
  first, then claimed frames plus what its source was served) makes room for a better one, never
  a claim several sources sent; a claim relayed first by a source that is then penalised stays,
  attributed to another source that sent it.
- **Hopeless claims** (fewer than 600 frames, the limit's floor, or more frames than the limit
  at the claim's own stamp) are refused before queueing: no replay (the Pi measurement: 200 junk
  300-frame claims cost 42.7 s of CPU, about 0.2 s of fixed cost per replay). On x86 the fixed
  cost of a replay is about 5 ms (`examples/replay_overhead.rs`: a 300-frame junk claim from a
  parent state 7.6 ms, `create_game` 1.5 ms, `load_state` 0.4 ms; from power-on 12 ms). Reusing
  emulators across replays would cut part of it but touches the replay path, so it is not done
  for the launch. Time-slicing replays (round-robin chunks across the queue head) is not done
  either; the reserved short-claim worker bounds an honest claim's wait instead.
- **Unknown sources share one budget.** Sources that never sent a valid claim share a replay
  budget of 8 long blocks (288,000 frames), refilled at 2 a minute, whatever their number (the
  Pi: 100 same-length 4,000-frame claims from 40 addresses cost 211 s of CPU). Claims beyond it
  are queued last (not refused), so they never delay a claim within it or from a known source,
  and are skipped once a valid block exists at their height.
  x86 testnet re-run (victim CPU, no honest block): 200 x 300 frames 0.07 s (hopeless, no
  replay), 60 x 36,000 frames 0.11 s (over the limit at their stamp), 100 x 4,000 frames from 40
  IPs 2.25 s (2.94 s before the shared budget; 7.7 s at the first queue version).
- **Final x86 run (testnet, 2026-11-25 20:10, victim at height 1, the honest block 2 of 2,643
  frames relayed by a fresh peer 3 s into the attack, 15 replay workers):**

  | attack | victim CPU | honest block accepted after |
  |---|---|---|
  | 60 long fakes (36,000 frames), 20 IPs | 11.6 s | 0.11 s |
  | 100 same-length fakes (4,000 frames), 40 IPs | 3.4 s | 0.12 s |
  | 200 short fakes (300 frames), 20 IPs | 0.6 s (no replay) | 0.16 s |
  | 100 long fakes, 100 IPs | 25.3 s | 0.18 s |
  | 50 runs of one key's template, 4 IPs | 0.6 s | 0.11 s |

  The long fakes here fit the limit at their stamp (the parent is half an hour old), so the
  ones that arrive before the honest block are replayed; after it they are skipped. A first
  version of the unknown-sources budget refused claims beyond it, and lost the honest block
  (relayed by a fresh peer): over-budget claims are now queued last instead.
- **Known sources first.** A claim from a source that never sent a valid claim counts 2,000
  frames longer when ordering, so a burst from fresh addresses queues behind a known peer's or
  miner's claim of the same length.
- **Unrestricted RPC** refuses requests with a foreign `Origin` (not in
  `--rpc-access-control-origins`) or a `Host` that is a name other than `localhost` (DNS
  rebinding: a web page must not reach `stop_daemon` or `submit_block`).

## What is left (not fixed here)

1. **(Mostly done: see "The replay queue".) Invalid claims from many addresses (b).** Each IP gets one or two replays per day (Monero's ban: score 10 per `bad_pow`, ban above 10). Each costs up to the current limit. Replays run on the P2P threads under `m_blockchain_lock`, so they serialize: 20 IPs delayed the honest block 7 s here, which is about 100 s on a Pi. Nothing short of consensus makes an invalid claim cost the sender something. A non-consensus improvement, not done because it is a real change to Monero's block path:
   - a verification worker with a bounded queue, ordered shortest claim first, then known peers before new ones;
   - replays done outside `m_blockchain_lock` (a replay only needs the parent's end state; the rules are re-checked under the lock in microseconds).

   Then an attacker can only delay an honest block with claims no longer than it, each costing no more than the honest replay.
2. **Replays hold locks.** The pre-pass holds the blockchain and txpool locks; every replay holds the blockchain lock. RPC calls that take them wait. Same remedy as 1.
3. **A fork rooted at a variant, longer than one sync span (20 blocks), would not reorganise.** Its first span doesn't reach above our tip, and its root is a variant of a block we have. This needs an attacker to partition miners for about 100 minutes. Fixing malleability at consensus (below) removes it.
4. **Budgets are per IP.** NAT'd honest peers share one; IPv6 makes addresses cheap; restricted RPC behind a proxy sees one address (then the global RPC budget governs). If the gateway proxies RPC, it should limit by the real client address itself.
5. **The leaderboard** scans the whole chain per entry, 30 times a minute. Fine at 35 blocks; at 10^5 blocks it is seconds per scan. Cache per-address counts incrementally before then.
6. **The RPC "busy" answers.** A restricted `submit_block` skipped by the budget can still return OK, the way Monero answers for alternative blocks; the block is not relayed. A `get_block_template` on an old, uncached `prev_block` when the RPC replay budget is spent reports the parent's progress as unknown (zero).

## Consensus questions for the owner (not shipped)

### 1. Proof canonicality: already enforced; malleability is elsewhere

- **Codecs 0-2.** The block id commits to the decoded run (`proof_id` hashes frames, start and inputs), so their encodings are interchangeable by design.
- **Codecs 3-5.** The decoded run must re-encode to exactly the body (`replay_state`: `dec.finish() != body` is rejected), so each run has one valid encoding.

All 34 mainnet blocks pass that check today; they verify under it. The variants are canonical encodings of *different runs*: the last 1-13 inputs differ, and the star still lands on the last frame. **No codec-level canonicality rule removes this**: a codec can't know which inputs mattered. Options:

- **(A) Bind the proof to its miner.** The template commits a miner key: a 32-byte public key the miner puts in the coinbase extra nonce (it is then in the seed). The block carries a signature over `(seed, proof_id)` with it, checked before the replay (~0.1 ms). Third parties can no longer mint variants. The miner can, but gains nothing, and they share its seed, so the policy dedups them.
  - Cost: a hard fork at an activation height; blocks below it are grandfathered (proofs unchanged, no signature).
  - Changes: the template and miner flow (SDK, web miner, `pow/miner`) to generate the key and sign; a 64-byte field.
  - **Recommended** for the next planned fork. It also removes the residual fork-race griefing: an attacker who sees block A first can race a variant A' to some miners, splitting the network for one block at no cost. Both pay the same miner, but the next block's miners on the losing side lose.
- **(B) Segwit-style ids.** The block id excludes the proof (it commits to the header and seed only); the proof is a witness, and the node keeps the first valid (or shortest) one per id. Variants collide on id and cost nothing. Pitfalls:
  - a bad witness must never mark the id invalid (Monero doesn't add `bad_pow` blocks to `m_invalid_blocks`, so this holds today);
  - `proof_id` vectors, explorers and wallets change;
  - proof choice among relayers needs a rule (the shortest, then the lowest hash).

  More invasive than (A).
- **(C) Do nothing at consensus.** The relay policy above already removes the DoS. What is left is (A)'s fork-race griefing and item 3 above. Acceptable for now.

### 2. A cheap admission ticket for every block (optional)

A small hashcash on the block id would make every claim cost the sender roughly what it costs a verifier: target ≈ 1 s of Keccak on a phone, checked in microseconds. It is the only thing that makes invalid-claim floods (b) cost the attacker. The price is a hard fork, about 1 s of extra work per mined block (web miners too), and a second "PoW" to explain. Recommended only if (b)-style floods are seen in practice; the queue in "left" item 1 should come first.

### 3. Small hardenings that are not consensus changes

They change no verdict:

- Lower the codecs' decode cap (`proof::MAX_FRAMES` = 2^20) to `max_frames_any()` (36,000) in `split_blob` / `decode_proof`. Any block claiming more is already invalid (the limit check), so verdicts don't change, but no code path can ever replay past it. Done here for the pre-pass only, so that no verdict anywhere changes on this branch.
- A power-on block in the middle of a sync span is valid only after a Grand Star, or more than 3,600 s after its parent. The pre-pass could check the gap before replaying it.
