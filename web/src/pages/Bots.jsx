import React from 'react';

const RUST = `use sm64coin_sdk::*;

struct MyBot;
impl Bot for MyBot {
    fn play(&mut self, env: &mut MiningEnv, stop: &dyn Fn() -> bool) -> anyhow::Result<()> {
        while !env.is_over() && !stop() {
            let played = env.step(GamePad::new(0, 0, 0))?; // reseed + perturbation applied
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
        played = env.step(GamePad(0, 0, 0))   # reseed + rng_pad applied
        mario = env.state()

practice(bot, seed=1, difficulty=1)                       # offline
run_miner(bot, "http://127.0.0.1:16481", "SM64...")       # against a daemon`;

export default function Bots() {
  return (
    <div>
      <div className="card">
        <h2>Write your own bot</h2>
        <p className="muted">
          The proof of work is playing Super Mario 64: from power-on, get a star within the time limit while the controller is
          randomly perturbed. A bot is anything that produces such a run. The Rust SDK (<span className="mono">pow/sdk</span>) and the
          Python SDK (<span className="mono">python/</span>) handle the perturbation, templates, verification and submission, so
          you write only the bot. Full guide: <span className="mono">BOTS.md</span>.
        </p>
      </div>

      <div className="card">
        <h3>The contract</h3>
        <p className="muted">A task is a 32-byte seed hash (<span className="mono">sm64_seed_hash</span> in the block template) and a difficulty, which fixes the frame limit and the perturbation. For every frame <span className="mono">f</span> (0-based) of a run from power-on:</p>
        <ol className="muted">
          <li>if <span className="mono">f &gt; 0</span> and <span className="mono">f % reseed_frames == 0</span>: <span className="mono">game.set_rng_seed(Seed.word(f / reseed_frames))</span>; before frame 0 set the seed to <span className="mono">word(0)</span> and <span className="mono">set_rng_config</span></li>
          <li><span className="mono">played = game.rng_pad(wanted)</span></li>
          <li><span className="mono">game.step_game(played)</span>, record <span className="mono">played</span></li>
        </ol>
        <p className="muted">
          <span className="mono">Seed.word(k)</span> is the little-endian 32-bit word <span className="mono">k % 8</span> of the seed hash, XOR <span className="mono">(k / 8) * 0x9E3779B9</span> (wrapping), with 0 becoming 1; <span className="mono">reseed_frames</span> is 300.
          The proof is the list of played pads, ending exactly on the frame a star is first collected, within <span className="mono">max_frames</span> (18000 at difficulty 1).
          Simulating futures on clones is fine, as long as committed frames go through the same perturbation.
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
          <span className="mono">practice()</span> runs your bot offline on random seeds and verifies a winning proof with the consensus verifier.
          Check a proof from any implementation with <span className="mono">verify_file &lt;seed_hash&gt; &lt;difficulty&gt; pads.bin</span> (raw 4 bytes a frame; <span className="mono">--conformance</span> checks only that the pads obey the perturbation, for runs that do not win yet).
        </p>
      </div>

      <div className="card">
        <h3>Tips</h3>
        <ul className="muted">
          <li>The game runs at tens of thousands of frames per second per core; use <span className="mono">env.fork()</span> and <span className="mono">save()/restore()</span> to try futures.</li>
          <li><span className="mono">env.state()</span> gives Mario's position, velocity, stars, level; read game memory for more.</li>
          <li>Replaying a recorded route fails within seconds: the perturbation pokes it off course (try <span className="mono">replay_bot</span>).</li>
          <li>The built-in miner follows a reference route: simulate many candidate continuations through the real perturbation, score progress along the route, commit the best first frames, repeat, with a beam of states (<span className="mono">pow/miner/src/beam.rs</span>).</li>
          <li>Check <span className="mono">stop()</span>: a new block makes a stale run worthless.</li>
        </ul>
      </div>
    </div>
  );
}
