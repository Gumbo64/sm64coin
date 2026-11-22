import React from 'react';

const RUST = `use sm64coin_sdk::*;

struct MyBot;
impl Bot for MyBot {
    fn play(&mut self, env: &mut MiningEnv, stop: &dyn Fn() -> bool) -> anyhow::Result<()> {
        while !env.is_over() && !stop() {
            let played = env.step(GamePad::new(0, 0, 0))?; // perturbation applied (a poke replaces the wanted pad)
            let mario = env.state()?;
        }
        Ok(())
    }
}
fn main() -> anyhow::Result<()> {
    run_miner(&mut MyBot, "http://127.0.0.1:16481", "SM64...", "baserom.us.z64")
}`;

const PY = `from sm64coin import GamePad, practice, run_miner

def bot(env, stop):
    while not env.is_over() and not stop():
        played = env.step(GamePad(0, 0, 0))   # perturbation applied
        mario = env.state()

practice(bot, seed=1, difficulty=1)                       # offline
run_miner(bot, "http://127.0.0.1:16481", "SM64...")       # against a daemon`;

export default function Bots() {
  return (
    <div>
      <div className="card">
        <h2>Write your own bot</h2>
        <p className="muted">
          The proof of work is playing Super Mario 64: each block is the game's next progress point (a star, a Bowser key, or the Grand Star
          after 70 stars), played on from the previous block's end state within a time limit while the controller is randomly perturbed.
          A bot is anything that produces such a run. The Rust SDK (<span className="mono">pow/sdk</span>) and the
          Python SDK (<span className="mono">python/</span>) handle the perturbation, templates, verification and submission, so
          you write only the bot. Full guide: <span className="mono">BOTS.md</span>.
        </p>
      </div>

      <div className="card">
        <h3>The 70-star cycle</h3>
        <p className="muted">
          A block's run starts from the <em>parent block's end state</em> (start byte 0) or from power-on (start byte 1). Power-on is used for the first block after genesis,
          after a block that ended a cycle (the Grand Star), and for any block timestamped more than 3600 s after its parent. The template says which:
          <span className="mono">sm64_start</span> is <span className="mono">parent</span>, <span className="mono">power-on</span> or <span className="mono">either</span>
          (also <span className="mono">sm64_power_on_at</span>, <span className="mono">sm64_parent_timestamp</span>, <span className="mono">sm64_parent_stars</span>, <span className="mono">sm64_parent_keys</span>).
          The parent's end state is the game after every frame of the current cycle: the RPC <span className="mono">get_sm64_cycle</span> returns <span className="mono">played_pads</span>
          (hex, 4 bytes a frame: u16 little-endian buttons, i8 x, i8 y), and replaying them unperturbed from power-on reaches it. The SDKs do this for you.
        </p>
        <p className="muted">
          Success: the game's progress (stars, Bowser keys, Grand Star with 70 stars) is one point more than at the start of the run, first on the last frame.
          The proof is <span className="mono">varint(frames) || start byte || codec || inputs of the non-poked frames</span>.
        </p>
      </div>

      <div className="card">
        <h3>The contract</h3>
        <p className="muted">
          A task is a 32-byte seed hash (<span className="mono">sm64_seed_hash</span> in the block template; it does not include the timestamp) and a time limit. The pokes are the same at every difficulty:
          5 bursts of 2 frames per 100 game-clock ticks. The game build is stock: the pokes are generated <em>outside</em> the game, from the seed (<span className="mono">pow/perturb</span>; the browser miner runs the same code as WASM).
          <span className="mono">SM64MiningGame</span> bundles a game with its perturbation (armed fresh at the start of every block's run). For every frame:
        </p>
        <ol className="muted">
          <li>read the game's clock (<span className="mono">gAreaUpdateCounter</span>) before the frame; a frame can be poked only if the clock moved since the previous frame's reading, so pausing neither spends nor rerolls pokes</li>
          <li>the perturbation picks the pad: in each window of <span className="mono">window_length</span> ticks there are exactly <span className="mono">random_amount</span> bursts of <span className="mono">burst_length</span> poked frames, each possible start taken with probability <span className="mono">remaining / remaining</span> (bursts left over frames left); a poked frame plays a random pad and ignores <span className="mono">wanted</span></li>
          <li><span className="mono">game.step_game(played)</span>, then, if the clock moved, mix a digest of the new game state into the perturbation, so pokes cannot be predicted without playing the run</li>
        </ol>
        <p className="muted">
          The generator is xoshiro256** seeded once from all of the seed hash; the code in <span className="mono">pow/perturb</span> is the specification.
          The proof stores only the inputs of the frames that were <em>not</em> poked: the verifier regenerates the pokes from the seed.
          Simulating futures on clones is fine, as long as committed frames go through the same perturbation: copy and save a <span className="mono">SM64MiningGame</span> (game and perturber together), never the game alone.
        </p>
      </div>

      <div className="card">
        <h3>Time limit, decay, timestamps</h3>
        <p className="muted">
          Difficulty is only the time limit: 18000 frames (10 minutes) at difficulty 1, about 15% shorter per doubling, floor 600 frames. After a 600 s grace the limit
          <em> grows every second</em> (one difficulty halving per 300 s), so a run longer than the current limit becomes valid later: ask the daemon with
          <span className="mono">get_block_template</span> and the parameter <span className="mono">sm64_frames: N</span>, which returns <span className="mono">sm64_valid_at</span>. Ties in chain weight are broken by the shorter run.
          The seed does not include the timestamp, so set the block's timestamp to "now" in the template blob right before submitting (rewrite the header's third varint; do not fetch a new template, new transactions would change the seed).
          The SDKs' <span className="mono">run_miner</span> waits for an over-budget run, stamps the block and submits, unless a new block arrives first.
        </p>
      </div>

      <div className="card">
        <h3>Rust</h3>
        <pre className="mono">{RUST}</pre>
        <p className="muted">
          Examples: <span className="mono">cargo run --release -p sm64coin-sdk --example random_bot | replay_bot | practice</span> (from <span className="mono">pow/</span>, ROM at <span className="mono">$SM64COIN_ROM</span> or <span className="mono">./baserom.us.z64</span>).
        </p>
      </div>

      <div className="card">
        <h3>Python</h3>
        <pre className="mono">{PY}</pre>
        <p className="muted">Install with <span className="mono">pip install -e python</span> (uses the <span className="mono">sm64-binds</span> package).</p>
      </div>

      <div className="card">
        <h3>Practice and test</h3>
        <p className="muted">
          <span className="mono">practice()</span> runs your bot offline on random seeds and verifies a winning proof with the consensus verifier (<span className="mono">Task::after</span> practises the next block from a finished run's end state).
          Check a proof from any implementation with <span className="mono">verify_file &lt;seed_hash&gt; &lt;difficulty&gt; pads.bin</span> (the pads played on every frame, raw 4 bytes each; <span className="mono">--blob</span> takes a sealed proof; <span className="mono">--conformance</span> checks only that the pads obey the perturbation, for runs that do not win yet).
        </p>
      </div>

      <div className="card">
        <h3>Tips</h3>
        <ul className="muted">
          <li>The game runs at tens of thousands of frames per second per core; use <span className="mono">env.fork()</span> and <span className="mono">save()/restore()</span> to try futures.</li>
          <li><span className="mono">env.state()</span> gives Mario's position, velocity, stars, level; read game memory for more.</li>
          <li>Replaying a recorded route fails within seconds: the perturbation pokes it off course (try <span className="mono">replay_bot</span>).</li>
          <li>Each block continues the previous one: after a star the game exits to the castle (and shows the save menu), so a bot needs a plan for wherever the game is. The built-in miner picks a route by which stars the save file already has (<span className="mono">pow/miner/src/rh.rs</span>).</li>
          <li>The older built-in controller follows a reference route: simulate many candidate continuations through the real perturbation, score progress along the route, commit the best first frames, repeat, with a beam of states (<span className="mono">pow/miner/src/beam.rs</span>).</li>
          <li>Check <span className="mono">stop()</span>: a new block makes a stale run worthless.</li>
        </ul>
      </div>
    </div>
  );
}
