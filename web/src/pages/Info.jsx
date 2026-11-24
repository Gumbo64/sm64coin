import React from 'react';
import { Link } from 'react-router';

const REPO = 'https://github.com/Gumbo64/sm64coin';

export default function Info() {
  return (
    <div className="card">
      <h2>Info</h2>

      <h3>Why are the controller inputs randomly poked?</h3>
      <p>
        Without the pokes, a run that worked once could be copied with tiny changes block after block. The random pokes,
        seeded by each block, mean every run has to be played anew: a recorded route falls apart, and only play that can
        recover from being knocked off course gets the star. Recovering is the sign of real skill, the same reason robotics
        researchers kick their walking robots.
      </p>

      <h3>How do I run a node?</h3>
      <p>
        Download the bundle for your computer from the <a href={`${REPO}/releases/latest`} target="_blank" rel="noreferrer">latest
        release</a>, put your own US Super Mario 64 ROM next to it as <span className="mono">baserom.us.z64</span>, and open
        the wallet app: it starts its own node. On a server, run <span className="mono">./sm64coin-node</span> (a systemd unit is
        included) and open TCP port 16480.
      </p>

      <h3>Can I run my own auto-miner?</h3>
      <p>
        Yes. Use the <Link to="/automine">Auto-mine</Link> page right here in the browser, press <b>Start</b> on the wallet
        app's Mining page, or run <span className="mono">sm64coin-automine</span> from the bundle. To write your own mining
        bot, see the Rust and Python SDKs in the <a href={`${REPO}#writing-your-own-bot`} target="_blank" rel="noreferrer">README</a>.
      </p>
    </div>
  );
}
