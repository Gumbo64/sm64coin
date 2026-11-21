# Writing a bot for SM64Coin

SM64Coin's proof of work is playing Super Mario 64: from power-on, get a star within the time limit while the controller is randomly perturbed. A **bot** is anything that produces such a run. This guide gives the contract, quick starts for Rust and Python, and how to test your work offline. Both SDKs build on the `sm64-binds` game library (a WASM build of the game, with clone and save states) and wrap the consensus rules so you only write the bot.

- Rust: `pow/sdk` (crate `sm64coin-sdk`, depends on `pow/sm64pow`, so the rules cannot drift).
- Python: `python/` (package `sm64coin`, on the `sm64-binds` Python package; keeps a copy of the rules that tests check against Rust).

## The contract

A task is a **seed hash** (32 bytes, `cn_fast_hash` of the block's hashing blob), a **difficulty**, and what the difficulty implies (`sm64pow::params(difficulty)`): `max_frames`, the perturbation settings and `reseed_frames` (300). The daemon's `get_block_template` returns them as `sm64_seed_hash`, `difficulty`, `sm64_max_frames`, `sm64_random_amount`, `sm64_window_length`, `sm64_burst_length`, `sm64_reseed_frames`.

Play a fresh game from power-on. For every frame `f` (0-based):

1. If `f > 0` and `f % reseed_frames == 0`: `game.set_rng_seed(Seed.word(f / reseed_frames))`. The game starts with `set_rng_seed(Seed.word(0))` and `set_rng_config(params.rng)` before frame 0.
2. `played = game.rng_pad(wanted)` where `wanted` is what your bot wants to press. On poked frames `played` is random; otherwise it equals `wanted`.
3. `game.step_game(played)`.
4. Record `played`.

`Seed.word(k)`: the 32-bit little-endian word `k % 8` of the 32-byte seed hash, XOR `(k / 8) * 0x9E3779B9` (wrapping 32-bit), and 0 becomes 1.

The **proof** is the list of played pads, ending exactly on the frame the success condition first holds (currently: any star, `num_stars > 0`), at most `max_frames` long. A proof with trailing frames after the first success is invalid. The raw encoding is 4 bytes per pad (u16 little-endian buttons, i8 stick x, i8 stick y); the blob is a codec byte (0 = raw) plus the codec's body. The daemon re-encodes to the smallest codec; the block id depends only on the decoded pads.

Simulating futures on clones or save states is allowed (it is how the built-in miner works), as long as the frames you commit went through exactly the same perturbation. The reseed is part of the game state of a run: a clone taken at frame 290 and stepped through frame 300 must reseed at 300 too.

Difficulty: 1 is the easiest (18000 frames = 10 minutes at 30 fps, 10 poked frames per 100: 5 bursts of 2). Each doubling shortens the limit by about 15% (floor 1800 frames) and adds one burst per 100 frames. The effective difficulty halves for every 300 s past 600 s since the parent block. The success condition and parameters are in `pow/sm64pow/src/rules.rs`; the SDKs refuse a daemon whose numbers differ from theirs.

## Rust quick start

```rust
use sm64coin_sdk::*;

struct MyBot;
impl Bot for MyBot {
    fn play(&mut self, env: &mut MiningEnv, stop: &dyn Fn() -> bool) -> anyhow::Result<()> {
        while !env.is_over() && !stop() {
            let wanted = GamePad::new(0, 0, 0); // your decision
            let played = env.step(wanted)?;     // reseed + perturbation applied
            let mario = env.state()?;           // pos, vel, num_stars, level_num, ...
        }
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    run_miner(&mut MyBot, "http://127.0.0.1:16481", "SM64...", "baserom.us.z64")
}
```

`MiningEnv`: `step(wanted) -> played`, `frame()`, `frames_left()`, `state()`, `is_success()`, `is_over()`, `pads()`, `fork()` (independent copy), `save()` / `restore(&snap)` (rewind), `copy_from(&other)`, `game_mut()` (read memory; never step it yourself), `proof()`, `proof_raw()`, `verify()`. `Task::from_template`, `Task::practice(seed, difficulty)`, `Task::new(Seed, difficulty)`. `run_miner` loops over templates, raises `stop` when a new block arrives or the difficulty decays, verifies the proof locally and submits it. `stop` only matters to long searches: check it now and then and return.

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
        played = env.step(GamePad(0, 0, 0))   # reseed + rng_pad applied
        mario = env.state()                   # GameState: pos, vel, numStars, levelNum, ...

print(practice(bot, seed=1, difficulty=1)["success"])        # offline
run_miner(bot, "http://127.0.0.1:16481", "SM64...")          # against a daemon
```

`Daemon(url).get_task(address)` returns `(Task, Template)`; `daemon.seal(template, env.proof())` and `daemon.submit(block_hex)` finish a block by hand. Python's `sm64-binds` has everything the contract needs (`rng_pad`, `set_rng_seed`, `set_rng_config`). It has no byte-level save states, only game copies, so `env.save()` is a clone. See `python/examples/`.

## Practice offline

No daemon needed: `practice(bot, seed, difficulty)` (Rust and Python) runs your bot on a local task and, on success, checks the proof with the consensus verifier (Rust: automatically). Try many seeds, and higher difficulties for harder perturbation and a shorter limit.

## Test your proof

```sh
cd pow && cargo build --release -p sm64coin-sdk --examples
target/release/examples/verify_file <seed_hash_hex> <difficulty> pads.bin          # exit 0 = VALID
target/release/examples/verify_file <seed_hash_hex> <difficulty> pads.bin --conformance   # only checks every pad is what the perturbation allows (for runs that do not win yet)
```

`pads.bin` is the raw 4-bytes-a-frame format (`env.proof_raw()` in Python; `env.proof_raw()` in Rust). The Python test suite does exactly this: a Python run that replays a real winning proof, and random runs across reseed boundaries, are checked with this Rust verifier (`pytest python/tests`). `checkrun` (in `pow/miner`) does the same for the web miner's debug JSON.

## Tips

- **Speed.** The game runs at tens of thousands of frames per second per core with no rendering (the bindings advertise ~80k in the best case; the SDK steps measured 15-25k in Rust and ~5k in Python here, mostly the per-frame state read and call overhead). Use `fork()` for what-ifs; spread searches over cores yourself (limit threads in shared environments).
- **Clones and save states.** `fork()` copies the game (cheapest for many short rollouts); `save()` / `restore()` rewinds one env. In Rust the lower-level `sm64_binds::SaveState` can be written to disk.
- **Reading Mario.** `env.state()` gives position, velocity, stars, level, course/act, health. For more (action, objects, floor) read game memory through `env.game_mut()`; the `obrrt-core` crate (`pow/vendor/obrrt-core`) has `read_state` with actions, surfaces and objects.
- **Replaying a route fails.** A recorded route (e.g. `pow/miner/routes/*.m64`) played blindly is poked within a few frames and diverges. `replay_bot` shows it.
- **Reference-route approach (built-in miner, `pow/miner/src/beam.rs`, `tracker.rs`).** Take an unperturbed route that reaches a star. It gives a target state per frame. Rolling-horizon: from the committed state, simulate many candidate continuations (route inputs time-shifted and mutated) on clones through the real perturbation (`sm64pow::mine_step`), score how far along the route each ends up and how close, commit the first few frames of the best, repeat. `beam.rs` keeps a beam of states instead of one line and re-seeds from an archive when stuck. Because candidates run through the exact perturbation, whatever is committed verifies.
- **A new block restarts everything.** Check `stop()`; a stale run is worthless.
