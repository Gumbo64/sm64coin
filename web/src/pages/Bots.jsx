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
        <h2>Build a bot</h2>
        <p className="muted">
          A block is the game's next progress point (a star, a Bowser key or the Grand Star), played on from the previous
          block's end state while the controller is randomly poked. A bot is anything that plays that run. The SDKs handle the
          pokes, templates, timing and submission; you write only the play. Full guide: <span className="mono">BOTS.md</span>.
        </p>
        <ul className="muted">
          <li>Shorter runs win: the time limit (up to 20 minutes of play) grows from the parent block's timestamp, so the shortest run becomes valid first.</li>
          <li>Try futures on copies with <span className="mono">env.fork()</span> and <span className="mono">save()/restore()</span>; a recorded route won't survive the pokes.</li>
          <li>Each block continues the last one, so plan from wherever the game is (the built-in miner: <span className="mono">pow/miner/src/rrt</span>).</li>
          <li>Check <span className="mono">stop()</span>: a new block makes your run worthless.</li>
        </ul>
      </div>

      <div className="card">
        <h3>Rust</h3>
        <pre className="mono">{RUST}</pre>
        <p className="muted">Examples: <span className="mono">cargo run --release -p sm64coin-sdk --example random_bot</span> (in <span className="mono">pow/</span>).</p>
      </div>

      <div className="card">
        <h3>Python</h3>
        <pre className="mono">{PY}</pre>
        <p className="muted">Install with <span className="mono">pip install -e python</span>.</p>
      </div>
    </div>
  );
}
