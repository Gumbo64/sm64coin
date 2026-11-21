# Writing a bot for SM64Coin

SM64Coin's proof of work is playing Super Mario 64: from power-on, get a star within the time limit while the controller is randomly perturbed. A **bot** is anything that produces such a run. This guide gives the contract, quick starts for Rust and Python, and how to test your work offline. Both SDKs build on the `sm64-binds` game library (a WASM build of the game, with clone and save states) and wrap the consensus rules so you only write the bot.

- Rust: `pow/sdk` (crate `sm64coin-sdk`, depends on `pow/sm64pow`, so the rules cannot drift).
- Python: `python/` (package `sm64coin`, on the `sm64-binds` Python package; keeps a copy of the rules that tests check against Rust).

## The contract

A task is a **seed hash** (32 bytes, `cn_fast_hash` of the block's hashing blob), a **difficulty**, and what the difficulty implies (`sm64pow::params(difficulty)`): `max_frames` and the perturbation settings (`window_length`, `random_amount` bursts of `burst_length`, button probabilities). The daemon's `get_block_template` returns them as `sm64_seed_hash`, `difficulty`, `sm64_max_frames`, `sm64_random_amount`, `sm64_window_length`, `sm64_burst_length`.

The game build is stock: **the pokes are generated outside the game**, from the seed, in the `perturb` crate (`pow/perturb`; the browser runs the same code compiled to WASM, Python has a line-by-line port in `python/src/sm64coin/perturb.py`). `sm64pow::MiningGame { game, pert }` bundles a game with its perturbation; use it for everything you play. Play a fresh game from power-on. For every frame, with `wanted` what your bot wants to press:

1. Read the game's **clock** (`gAreaUpdateCounter`, a `u16`) *before* the frame.
2. The perturbation decides the pad. It can poke only if the clock moved since the previous frame's reading (so pausing and menus neither spend nor reroll pokes; there is no explicit rule about pausing). Pokes are scheduled in windows of `window_length` clock-ticking frames: exactly `random_amount` bursts of `burst_length` frames per window, each frame that may start a burst starting one with probability `remaining bursts / remaining frames of the window` (so the budget is always spent). A poked frame plays a random pad (A, B, Z with probabilities 0.5, 0.5, 0.2, and both stick axes uniform in -80..80) and ignores `wanted`; otherwise `played = wanted`.
3. `game.step_game(played)`.
4. If the clock moved during the frame, **mix a digest of the new game state** (Mario's position, velocity, action, angles, health, coins, stars, Lakitu's yaw, the global timer, the game's own RNG, level and area) into the perturbation. So the next pokes depend on everything that happened, and cannot be predicted without playing the run.
5. Record `played`.

The generator is xoshiro256** seeded once from all 256 bits of the seed hash (there is no reseeding), mixed with splitmix64 and the digest. The exact algorithm and memory regions are in `pow/perturb/src/lib.rs` and `pow/perturb/src/sm64.rs`; that code is the specification. In Rust this is `sm64pow::MiningGame::step(wanted) -> (played, poked)` (also `sm64pow::play_frame` / `mine_step`); you do not implement any of it yourself.

The **proof** stores only what the player decided: `varint(frames) || codec byte || codec body`, where the body holds the inputs (`wanted`) of the frames that were **not poked**, in order, 4 bytes each in codec 0 (u16 little-endian buttons, i8 stick x, i8 stick y). Poked frames are not stored: the verifier regenerates the pokes from the seed. The run must end exactly on the frame the success condition first holds (currently: any star, `num_stars > 0`), at most `max_frames` long, and use every input; a proof with trailing frames after the first success is invalid. The daemon re-encodes to the smallest codec; the block id depends only on the decoded proof. If you only keep the pads played on every frame, `sm64pow::seal_proof(seed, params, played)` (Rust; `env.proof()` in the SDKs) replays them and drops the poked ones.

Simulating futures on clones or save states is allowed (it is how the built-in miner works), as long as the frames you commit went through exactly the same perturbation. **The perturbation is part of the state of a run**: copy, fork and save a `MiningGame` (`fork()`, `sync_from()`, `save()`/`load()`), never its inner `game` alone, or the clone will draw different pokes than the run you commit. (The SDKs' `fork()`, `save()` and `restore()` do this for you.)

Difficulty: 1 is the easiest (18000 frames = 10 minutes at 30 fps, 10 poked frames per 100: 5 bursts of 2). Each doubling shortens the limit by about 15% (floor 1800 frames) and adds one burst per 100 frames. The effective difficulty halves for every 300 s past 600 s since the parent block. The success condition and parameters are in `pow/sm64pow/src/rules.rs`; the SDKs refuse a daemon whose numbers differ from theirs.

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

`MiningEnv` (wraps a `MiningGame`): `step(wanted) -> played`, `frame()`, `frames_left()`, `state()`, `is_success()`, `is_over()`, `pads()`, `fork()` (independent copy), `save()` / `restore(&snap)` (rewind), `copy_from(&other)`, `game_mut()` (read memory; never step it yourself), `proof()` (the sealed blob), `proof_raw()` (every played pad), `verify()`. `Task::from_template`, `Task::practice(seed, difficulty)`, `Task::new(Seed, difficulty)`. `run_miner` loops over templates, raises `stop` when a new block arrives or the difficulty decays, verifies the proof locally and submits it. `stop` only matters to long searches: check it now and then and return.

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

`Daemon(url).get_task(address)` returns `(Task, Template)`; `daemon.seal(template, env.proof())` and `daemon.submit(block_hex)` finish a block by hand. The Python SDK reimplements the perturbation (`sm64coin.perturb`, reading the game's memory through `sm64-binds`) and seals the proof itself; the tests check both against the Rust verifier. The `sm64-binds` Python package must be the one with the current game build (reinstall it from `sm64-binds/python` after a game rebuild). It has no byte-level save states, only game copies, so `env.save()` is a clone (game plus perturbation). See `python/examples/`.

## Practice offline

No daemon needed: `practice(bot, seed, difficulty)` (Rust and Python) runs your bot on a local task and, on success, checks the proof with the consensus verifier (Rust: automatically). Try many seeds, and higher difficulties for harder perturbation and a shorter limit.

## Test your proof

```sh
cd pow && cargo build --release -p sm64coin-sdk --examples
target/release/examples/verify_file <seed_hash_hex> <difficulty> pads.bin          # exit 0 = VALID
target/release/examples/verify_file <seed_hash_hex> <difficulty> pads.bin --conformance   # only checks every pad is what the perturbation allows (for runs that do not win yet)
target/release/examples/verify_file <seed_hash_hex> <difficulty> proof.bin --blob   # a sealed proof: varint(frames) + codec + non-poked inputs
```

`pads.bin` is the pads played on **every** frame, raw 4-bytes-a-frame (`env.proof_raw()` in Python and Rust); `proof.bin` is the sealed blob (`env.proof()`) that goes in a block. The Python test suite checks both: a Python run that replays a real winning run, and random runs full of pokes, are verified with this Rust verifier (`pytest python/tests`). `checkrun` (in `pow/miner`) does the same for the web miner's debug JSON (it takes the pads played).

## Tips

- **Speed.** The game runs at tens of thousands of frames per second per core with no rendering (the bindings advertise ~80k in the best case; the SDK steps measured 15-25k in Rust and ~5k in Python here, mostly the per-frame state read and call overhead). Use `fork()` for what-ifs; spread searches over cores yourself (limit threads in shared environments).
- **Clones and save states.** `fork()` copies the game (cheapest for many short rollouts); `save()` / `restore()` rewinds one env. In Rust the lower-level `sm64_binds::SaveState` can be written to disk.
- **Reading Mario.** `env.state()` gives position, velocity, stars, level, course/act, health. For more (action, objects, floor) read game memory through `env.game_mut()`; the `obrrt-core` crate (`pow/vendor/obrrt-core`) has `read_state` with actions, surfaces and objects.
- **Replaying a route fails.** A recorded route (e.g. `pow/miner/routes/*.m64`) played blindly is poked within a few frames and diverges (a route is a sequence of `wanted` pads; replay it through `MiningGame::step`). `replay_bot` shows it.
- **Reference-route approach (built-in miner, `pow/miner/src/beam.rs`, `tracker.rs`).** Take an unperturbed route that reaches a star. It gives a target state per frame. Rolling-horizon: from the committed state, simulate many candidate continuations (route inputs time-shifted and mutated) on `MiningGame` forks through the real perturbation (`sm64pow::mine_step`), score how far along the route each ends up and how close, commit the first few frames of the best, repeat. `beam.rs` keeps a beam of states instead of one line and re-seeds from an archive when stuck. Because candidates run through the exact perturbation, whatever is committed verifies.
- **A new block restarts everything.** Check `stop()`; a stale run is worthless.
