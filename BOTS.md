# Writing a bot for SM64Coin

SM64Coin's proof of work is playing Super Mario 64: each block is the game's **next progress point** (a new star, a Bowser key, a cap switch, or the Grand Star once you have 70 stars), played on from the end state of the previous block, within a time limit, while the controller is randomly perturbed. After the Grand Star the cycle ends and the next block starts again from power-on (the "70-star cycle"). A **bot** is anything that produces such a run. This guide gives the contract, quick starts for Rust and Python, and how to test your work offline. Both SDKs build on the `sm64-binds` game library (a WASM build of the game, with clone and save states) and wrap the consensus rules so you only write the bot.

- Rust: `pow/sdk` (crate `sm64coin-sdk`, depends on `pow/sm64pow`, so the rules cannot drift).
- Python: `python/` (package `sm64coin`, on the `sm64-binds` Python package; keeps a copy of the rules that tests check against Rust).

## The contract

### Where a run starts

A block's run starts from one of two states:

- **Parent** (`start byte 0`, the normal case): the game state at the end of the parent block's run. Replaying every frame the cycle has played (unperturbed: the pads actually played, pokes included) from power-on reaches exactly that state. The daemon serves them: JSON-RPC `get_sm64_cycle {hash?}` (default: the tip) returns `{blocks: [{height, hash, frames, power_on}], played_pads, stars, keys, grand_star, ends_cycle}`, `played_pads` being hex, 4 bytes per frame (u16 LE buttons, i8 stick x, i8 stick y) from the cycle's first block to that tip. Both SDKs do the replay for you (`Task::from_template` / `Daemon.get_task`); cache the resulting state per tip.
- **Power-on** (`start byte 1`): a fresh game. Allowed for the first block after the genesis block, after a block that ended a cycle (the Grand Star), and as a stall escape for a chain stuck on a state nobody can progress from: a restart is judged against the limit as it stood `delay` seconds earlier, delay = 4 h + 240 s per point of progress the restart throws away, at most 8 h (v0.3; `rules.rs` restart_*; the template's `sm64_power_on_at` says when).

The template says which is allowed: `sm64_start` is `"parent"`, `"power-on"` or `"either"`, with `sm64_power_on_at` (unix time from which power-on is allowed), `sm64_parent_timestamp`, `sm64_parent_stars`, `sm64_parent_keys`, `sm64_cycle_start_height`.

### Success

`rules().is_success(start_progress, now_progress)`: the game's progress (`Progress { stars, keys, grand_star }`, read from Mario's star counter, the save file's key and door flags, and the Grand Star cutscene with at least 70 stars; `pow/perturb/src/sm64.rs`, `points() = stars + keys + grand_star`) is one point more than at the start of the run. It must first hold on the last frame of the run: a proof with frames after the first success is invalid. In Rust `env.is_success()` / `env.progress()`; in Python the same.

### The task and the perturbation

A task is the **seed hash** (32 bytes, `cn_fast_hash` of the block's hashing blob; it does not include the timestamp) and the time limit. The pokes are fixed at every difficulty: 5 bursts of 2 frames per 100 game-clock ticks. Difficulty only sets the time limit.

The game build is stock: **the pokes are generated outside the game**, from the seed, in the `perturb` crate (`pow/perturb`; the browser runs the same code compiled to WASM, Python has a line-by-line port in `python/src/sm64coin/perturb.py`). `sm64pow::SM64MiningGame { game, pert }` bundles a game with its perturbation; use it for everything you play. Start it with `sm64pow::chain::mining_start(start, Some(&parent_state) or None, &seed, &params)`. For every frame, with `wanted` what your bot wants to press:

1. Read the game's **clock** (`gAreaUpdateCounter`, a `u16`) *before* the frame.
2. The perturbation decides the pad. It can poke only if the clock moved since the previous frame's reading (so pausing and menus neither spend nor reroll pokes). Pokes are scheduled in windows of `window_length` clock-ticking frames: exactly `random_amount` bursts of `burst_length` frames per window, each frame that may start a burst starting one with probability `remaining bursts / remaining frames of the window`. A poked frame plays a random pad (A, B, Z with probabilities 0.5, 0.5, 0.2, and both stick axes uniform in -80..80) and ignores `wanted`; otherwise `played = wanted`.
3. `game.step_game(played)`.
4. If the clock moved during the frame, **mix a digest of the new game state** into the perturbation, so the next pokes depend on everything that happened and cannot be predicted without playing the run.
5. Record `played`.

The generator is xoshiro256** seeded once from all 256 bits of the seed hash, mixed with splitmix64 and the digest. The exact algorithm and memory regions are in `pow/perturb/src/lib.rs` and `pow/perturb/src/sm64.rs`; that code is the specification. In Rust this is `sm64pow::SM64MiningGame::step(wanted) -> (played, poked)`; you do not implement any of it yourself. Note the perturbation starts fresh at the start of every block's run (from the block's own seed), whatever state the run starts from.

### The proof

v0.4 layout (committed inputs, docs/V0.4-DESIGN.md section 1): `varint(frames) || start byte (0 = parent, 1 = power-on) || input_hash (32 bytes) || encoding`, with `encoding = codec byte || codec body`. The **committed header** (`frames`, `start`, `input_hash`) is what consensus reads; the encoding is **transport** and any encoding that decodes to the committed inputs is fine. `input_hash = keccak256("sm64run2" || frames u32 LE || start u8 || pad_1 || ... || pad_n)` over the non-poked frames' inputs (`pad` = buttons u16 LE, stick x i8, stick y i8). **`proof_id` is the input hash**, read from the header with no decoding; it is what the block id and your signature commit to (so re-encoding a run never changes its block id or signature). A header that does not parse gets `keccak256("sm64bad!" || blob)`.

The body holds the inputs (`wanted`) of the frames that were **not poked**, in order, 4 bytes each in codec 0. Poked frames are not stored: the verifier regenerates the pokes from the seed. The run must end exactly on the frame the success condition first holds, use every input, and be within the time limit. A blob whose encoding does not decode to its committed hash is a **bad encoding**: the node drops the blob and its sender and never marks the block invalid. **Codecs are transport**: every v0.4 node decodes the baseline 0 (raw), 1 (run-length) and 2 (xz); state-aware codecs 3-7 (decoding means replaying, `sm64pow::decode_proof_any`) are negotiated per peer (`NOTIFY_SM64_CODECS`), and nodes transcode when relaying to the smallest encoding the peer decodes, under the same block id. `get_block` returns the stored blob plus `sm64_input_hash` (hex), `sm64_codec` (the stored encoding's codec id, -1 none) and `sm64_records_bytes` (the length of the trailing `sm64_proofs` field, see below); unrestricted RPC takes `sm64_codec` 0-2 to ask for a specific stateless encoding. Python and browser bots keep submitting codec 0 (or 1). If you only keep the pads played on every frame, `sm64pow::seal_proof_at(start, parent_state, seed, params, played)` (Rust; `env.proof()` in the SDKs) replays them, drops the poked ones, computes the input hash and writes the committed header.

Simulating futures on clones or save states is allowed (it is how the built-in miner works), as long as the frames you commit went through exactly the same perturbation. **The perturbation is part of the state of a run**: copy, fork and save a `SM64MiningGame` (`fork()`, `sync_from()`, `save_state()`/`load_state()`), never its inner `game` alone. (The SDKs' `fork()`, `save()` and `restore()` do this for you.)

### Time limit, decay and ties

Difficulty is only the time limit: 18000 frames (10 minutes at 30 fps) at difficulty 1, shrinking by 2 % per eighth of a doubling (about 15 % per doubling), floor 600 frames. The limit **grows continuously** once a block is more than 600 s old: after the grace period every second adds a little, one halving of the difficulty per 300 s (`rules().max_frames(retarget_difficulty, seconds_since_parent)`, integer arithmetic; `sm64_retarget_difficulty` in the template). So a run longer than the current limit is not lost: it becomes valid later. Ask the daemon when: `get_block_template` with the request parameter `sm64_frames: N` returns `sm64_valid_at`, the unix time from which a run of N frames is valid. Chain weight (what the best chain is) counts the effective difficulty, and blocks with the same work are tie-broken by the shorter run (fewer frames).

### Signed proofs (v0.3)

On the v0.3 chain every block except genesis is **signed by a per-template miner key**; old-style unsigned blocks are rejected. This stops anyone from re-publishing your run with a few different trailing inputs as a rival block.

1. Before playing, generate a throwaway key (Rust `Keypair::generate()`, Python `Keypair.generate()`).
2. Request the template with `get_block_template {wallet_address, extra_nonce: hex(0x64 || public key)}` (`key.extra_nonce()`), and **no** `reserve_size`. The daemon puts the extra nonce in the coinbase, so `sm64_seed_hash` (the perturbation seed) already covers your key. The key must be fixed before the run is played, and every template call for that run (the `sm64_frames` valid-at queries too) should pass the same `extra_nonce`.
3. When the run is done, sign it: Rust `sign_proof(&key, &template, &proof)`, Python `sign_proof(key, seed, proof)`. The signature is Monero's Schnorr signature over `keccak256("sm64coin proof signature v1" || seed || proof_id || frames u32 LE)`; in v0.4 `proof_id` is the committed `input_hash` (the same whichever codec carries the run).
4. Seal: the v0.4 template blob ends with a **66-byte tail**: `00` (empty proof), 64 zero bytes (empty `sm64_sig`) and `00` (the empty `sm64_proofs` field); `seal` replaces the first 65 with `varint(len(proof)) || proof || sig` and keeps the last byte. (The daemon reattaches the included-proofs field by its root at `submit_block`, so submit with it empty.) Only the final block bytes change, so signing after the timestamp rewrite is fine.

**One run per key.** A template's key signs at most ONE run for that seed. A second, different signed run under the same key is equivocation evidence, and honest nodes then avoid your block. So do not publish at find and do not re-send improvements: keep searching and improving **privately**, and submit your best run exactly once, a few seconds (`submit_lead`, default 5) before the second it becomes valid, stamped with that second (nodes hold a block until its run fits, and do not relay a run before it is due and replayed, so sending early gains nothing). A run that is already valid is sent at once. At equal frames between two miners inside the 30 s window, nodes prefer the smaller proof (then the lower signature hash): submit your smallest encoding. For the stateless codecs (raw, run-length, xz) nodes measure the run-length encoding of your inputs whatever you send, so simpler inputs (fewer changes) win; for the state codecs (3-5) the blob itself counts. Keep the lead short: restricted (public) RPC refuses a run due more than 30 s after it arrives ("submitted too early"), so the SDKs cap `submit_lead` at 25 s; and the node answers `submit_block` with an error when it won't queue your block (a second run for the seed, a sibling that can't displace the tip, a full queue), not OK. Remember which seeds you signed (seed -> proof id) somewhere that survives a restart and check it BEFORE signing; signing the very same run again is harmless (the signature is deterministic), a different one never. If the node builds around an equivocated tip, the tip is not the template's `prev_hash`: read `get_info`'s `top_block_hash` before requesting the template and treat "the tip moved from that" as a new block ([docs/V0.3-NEW-CHAIN.md](docs/V0.3-NEW-CHAIN.md)).

`run_miner` does all of this with a fresh key per template, submits the one run `submit_lead_secs` (Rust `MinerConfig`, default 5; Python `submit_lead=5`) before it is valid, and keeps the signed-seed record in `signed_file` (Rust `MinerConfig::signed_file`, Python `signed_file=`; default `$SM64COIN_SIGNED_FILE` or `sm64coin-signed.json`; `""` keeps it in memory), so a restart never signs a second run for a seed. With your own keys, do not sign two runs for one shared key either: the group should agree on one submitter. To use your own keys (a group leader sharing the per-template secret with its members, who each can then sign and submit): Rust `run_miner_keyed(bot, &cfg, &mut |round| Ok(Keypair::from_secret(&secret)?))`, Python `run_miner(..., next_key=lambda round: Keypair(secret))`. Python's `proof_id` handles the stateless codecs (raw, RLE, xz) only, which is what the Python SDK produces.

### Included proofs, payouts and timing (v0.4)

A **proof for height h** is a signed run for a template on the same parent as block h that would have been valid but for the time limit: a displaced block, a runner-up, any miner's near-best run. Block h+1 may carry up to 64 of them in its trailing `sm64_proofs` field (`varint(n) || records`, after the 64-byte `sm64_sig`; wire format in docs/V0.4-DESIGN.md section 2.2), counted for fork choice ("longer AND heavier") and paid by block h+1's coinbase. Publish yours when it cannot win: seal it exactly like a block (template with the key, committed proof, signature) and send it with the RPC **`submit_sm64_proof {blob: hex}`** (the same hex as `submit_block`) as soon as block h is known. Result `1` new (pooled and relayed), `0` ignored (known, wrong height, outside the proof window of +60 s of auction time after the winner's due time, pool full), `-1` invalid, `-3` bad encoding, `-2` the parent's state is not available. One proof per key, and a run identical to another (same `input_hash`) counts once. `automine` does this itself on a new block while waiting, or when its block is refused. The proof must be a different key from block h's winner, and the proof is bound to the template's payout address (below).

- **Payout address**: the template commits its payout address (and the included-proofs root) in the coinbase extra nonce (`0x64 || key`, then TLVs: tag 1 payout flag || spend || view, tag 2 root). `get_block_template` takes `sm64_payout_address` (a standard address or a subaddress) for the payout TLV; the coinbase miner output still pays `wallet_address`. An included proof is paid by an extra coinbase output to that address, derived from public data (wallets find it like any payment). A proof record links your run to that address, so use a **fresh subaddress per template**: `automine --address <main> --payout-view-secret <hex>` derives subaddress (0, k) with k from a counter kept next to `--signed-file` (`sm64pow_subaddress`, `sm64pow::payout::subaddress`).
- **When to take a template**: `get_block_template` returns `sm64_records_ready_at` (unix seconds): the tip's eligible time plus the 30 s displacement window plus a 10 s collection grace. Build the template for a new tip at or after that time, so it includes the height's proofs (the proof set is frozen in the seed); the Rust miner waits for it.

### Timestamps

The seed does not include the timestamp, so **set the block's timestamp to "now" right before submitting**, in the template blob itself (`varint major_version, varint minor_version, varint timestamp, prev_id, nonce, ...`; rewrite the third varint) rather than fetching a new template (new mempool transactions would change the seed). The limit that applies is the one at the block's timestamp. Both SDKs: `seal(template_hex, proof, sig, timestamp)` (the signature is covered next; the timestamp is not signed).

A miner that finds an over-budget run therefore waits until `sm64_valid_at` (less `submit_lead`), stamps the block with that second, and submits once, unless the tip changes first (then the run is worthless). `run_miner` does this.

The success condition and parameters are in `pow/sm64pow/src/rules.rs`; the SDKs refuse a daemon whose numbers differ from theirs.

## Rust quick start

```rust
use sm64coin_sdk::*;

struct MyBot;
impl Bot for MyBot {
    fn play(&mut self, env: &mut MiningEnv, stop: &dyn Fn() -> bool) -> anyhow::Result<()> {
        while !env.is_over() && !stop() {
            let wanted = GamePad::new(0, 0, 0); // your decision
            let played = env.step(wanted)?;     // perturbation applied (a poke replaces `wanted`)
            let mario = env.state()?;           // pos, vel, num_stars, level_num, ...
        }
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    run_miner(&mut MyBot, "http://127.0.0.1:16481", "SM64...", "baserom.us.z64")
}
```

`MiningEnv` (wraps a `SM64MiningGame`): `step(wanted) -> played`, `frame()`, `frames_left()`, `state()`, `progress()`, `is_success()`, `is_over()`, `pads()`, `fork()` (independent copy), `save()` / `restore(&snap)` (rewind), `copy_from(&other)`, `game_mut()` (read memory; never step it yourself), `end_state()` (the game's save state: the next block's parent state), `proof()` (the sealed blob, with the start byte), `proof_raw()` (every played pad), `verify()`. `Task::from_template(&template, &daemon)` (rebuilds the parent's end state from `get_sm64_cycle`, or powers on), `Task::practice(seed, difficulty)` / `Task::new(Seed, difficulty)` (power-on), `Task::after(parent_state, progress, seed, difficulty)` (the next block of a cycle, offline), `task.allow_over_budget()`. `run_miner` loops over templates, raises `stop` when a new block arrives, checks the proof locally, waits if the run is over the current limit, signs the one run (recorded per seed) and submits it `submit_lead_secs` before it is valid. `stop` only matters to long searches: check it now and then and return. A bot's `play` sees `env.task().start` and `env.progress()`: it has to make the *next* progress from wherever the game is (after a star the game is in the castle, not at power-on).

Examples (run from the repository root, which holds `baserom.us.z64`; or set `SM64COIN_ROM`):

```sh
cd pow
cargo run --release -p sm64coin-sdk --example random_bot      # the API, random inputs
cargo run --release -p sm64coin-sdk --example replay_bot      # replays an m64; watch the perturbation break it
cargo run --release -p sm64coin-sdk --example practice -- --seeds 5 --difficulty 1
```

## Python quick start

```sh
pip install -e python            # pulls sm64-binds (and wasmtime)
```
```python
from sm64coin import GamePad, MiningEnv, practice, run_miner

def bot(env, stop):
    while not env.is_over() and not stop():
        played = env.step(GamePad(0, 0, 0))   # perturbation applied
        mario = env.state()                   # GameState: pos, vel, numStars, levelNum, ...

print(practice(bot, seed=1, difficulty=1)["success"])        # offline
run_miner(bot, "http://127.0.0.1:16481", "SM64...")          # against a daemon
```

`Daemon(url).get_task(address, key)` (key: `Keypair`, default a fresh one) returns `(Task, Template)` (it fetches `get_sm64_cycle` and replays it for a parent start; `start="power-on"` overrides); `Daemon.cycle()`, `Daemon.valid_at(address, key, frames)`; `sign_proof(key, seed, proof)`, `seal(template_hex, proof, sig, timestamp)` (or `daemon.seal(template, proof, ts)`, which signs with the template's key) and `daemon.submit(block_hex)` finish a block by hand. `run_miner` waits for over-budget runs and stamps the block. The Python SDK reimplements the perturbation (`sm64coin.perturb`, reading the game's memory through `sm64-binds`) and seals the proof itself; the tests check both against the Rust verifier. The `sm64-binds` Python package must be the one with the current game build (reinstall it from `sm64-binds/python` after a game rebuild). It has no byte-level save states, only game copies, so `env.save()` is a clone (game plus perturbation). See `python/examples/`.

## Practice offline

No daemon needed: `practice(bot, seed, difficulty)` (Rust and Python) runs your bot on a local task and, on success, checks the proof with the consensus verifier (Rust: automatically). Try many seeds, and higher difficulties for a shorter limit. `Task::after` (Rust) and `Task.practice(..., start_pads=...)` (Python) practise the next block of a cycle from a finished run's end state.

## Test your proof

```sh
cd pow && cargo build --release -p sm64coin-sdk --examples
target/release/examples/verify_file <seed_hash_hex> <difficulty> pads.bin          # exit 0 = VALID
target/release/examples/verify_file <seed_hash_hex> <difficulty> pads.bin --conformance   # only checks every pad is what the perturbation allows (for runs that do not win yet)
target/release/examples/verify_file <seed_hash_hex> <difficulty> proof.bin --blob   # a sealed power-on proof: varint(frames) + start + codec + non-poked inputs
```

`verify_file` checks runs from power-on (the first block of a cycle); a run from a parent state is checked by the SDKs (`env.verify()`, `task.check(blob)`) or by the daemon. `pads.bin` is the pads played on **every** frame, raw 4-bytes-a-frame (`env.proof_raw()` in Python and Rust); `proof.bin` is the sealed blob (`env.proof()`) that goes in a block. The Python test suite checks both: a Python run that replays a real winning run, and random runs full of pokes, are verified with this Rust verifier (`pytest python/tests`). `checkrun` (in `pow/miner`) does the same for the web miner's debug JSON (it takes the pads played).

## Tips

- **Speed.** The game runs at tens of thousands of frames per second per core with no rendering (the bindings advertise ~80k in the best case; the SDK steps measured 15-25k in Rust and ~5k in Python here, mostly the per-frame state read and call overhead). Use `fork()` for what-ifs; spread searches over cores yourself (limit threads in shared environments).
- **Clones and save states.** `fork()` copies the game (cheapest for many short rollouts); `save()` / `restore()` rewinds one env. In Rust the lower-level `sm64_binds::SaveState` can be written to disk.
- **Reading Mario.** `env.state()` gives position, velocity, stars, level, course/act, health. For more (action, objects, floor) read game memory through `env.game_mut()`; the `obrrt-core` crate (`pow/vendor/obrrt-core`) has `read_state` with actions, surfaces and objects.
- **Replaying a route fails.** A recorded route (e.g. `pow/miner/routes/*.m64`) played blindly is poked within a few frames and diverges (a route is a sequence of `wanted` pads; replay it through `SM64MiningGame::step`). `replay_bot` shows it.
- **Reference-route approach (the older built-in controller, `pow/miner/src/beam.rs`, `tracker.rs`; power-on first star only; the default controller is now the rolling-horizon GA of `rh.rs`).** Take an unperturbed route that reaches a star. It gives a target state per frame. Rolling-horizon: from the committed state, simulate many candidate continuations (route inputs time-shifted and mutated) on `SM64MiningGame` forks through the real perturbation (`sm64pow::mine_step`), score how far along the route each ends up and how close, commit the first few frames of the best, repeat. `beam.rs` keeps a beam of states instead of one line and re-seeds from an archive when stuck. Because candidates run through the exact perturbation, whatever is committed verifies.
- **Boot and settle are free speed.** From power-on the game needs about 1646 frames to hand Mario over (title screen, file select, the 1478-frame intro cutscene whose two-page closing dialog takes a press per page); after a star, about 355 (the dance, the save menu whose A only works once its timer passes 110, a milestone dialog). A schedule that presses Start at the title screen's first Start-taking frame (117) and A only on a showing dialog page, moves the save menu's cursor back to "save and continue" and stops at the first controllable frame gains 36 frames (mean 52 counting the wait) from power-on over the fixed schedule the miners shipped with (A on alternate frames, Start from 150, then waiting until Mario stands still); a faster boot wins power-on blocks in a shortest-run auction, so use it. Rust bots call `sm64coin_sdk::macros::boot(&mut env)` / `settle(&mut env)`; `automine` and the web miner play the same logic (`sm64coin_miner::macros`; `--legacy-macros` / `?legacy_macros=1` bring back the old schedule). The macros read the game's state every frame (poked frames replace the whole pad, so a press can be lost or a cursor moved) and are a function of the state only, never of the seed. Python bots write their own (read the menu cursor `gDialogLineNum`, `gDialogBoxState`, `gCourseDoneMenuTimer` through `sm64-binds`) and follow `pow/miner/src/macros.rs`, which is the reference.
- **Each block continues the previous one.** After a star the game exits to the castle (and shows the save menu: press A). The built-in miner (`pow/miner/src/rh.rs`) keeps a small route library keyed by which stars the save file already has (today: Bob-omb Battlefield's Chain Chomp star and King Bob-omb star, then it has nothing for further blocks until the power-on stall escape); a bot needs a plan for every progress point it wants to make.
- **A new block restarts everything.** Check `stop()`; a stale run is worthless (unless it is waiting for the limit to grow and the tip has not moved).
