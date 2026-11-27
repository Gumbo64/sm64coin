import React, { useEffect, useState } from 'react';
import { Link } from 'react-router';
import { daemon, getNode, getPublicNode, usingPublicFallback } from '../lib/rpc.js';
import { probeChain } from '../lib/nodechain.js';
import { latestRelease, SITE_VERSION } from '../lib/updates.js';
import { REPO, describeNode, updateAvailable, versionText, shortSha, commitUrl } from '../lib/versions.js';

const GIT_SHA = import.meta.env.VITE_GIT_SHA || '';
const CHAIN_TEXT = { ok: 'on this chain', 'wrong-chain': 'wrong chain (an old v0.2 node?)', down: 'unreachable' };

// Versions of the website and of the node it talks to, and whether a newer release is out.
function Versions() {
  const [node, setNode] = useState(null);   // describeNode(...) once known
  const [chain, setChain] = useState(null); // 'ok' | 'wrong-chain' | 'down'
  const [latest, setLatest] = useState(null);
  useEffect(() => {
    let dead = false;
    (async () => {
      const [info, ver, probe, rel] = await Promise.all([
        daemon('get_info').catch(() => null), daemon('get_version').catch(() => null),
        probeChain(daemon), latestRelease(),
      ]);
      if (dead) return;
      setNode(info || ver ? describeNode(info, ver) : null);
      setChain(probe.state);
      setLatest(rel);
    })();
    return () => { dead = true; };
  }, []);

  const url = getNode() || 'this site\u2019s proxy';
  const which = usingPublicFallback() ? 'public node' : getNode() && getNode() === getPublicNode() ? 'public node' : 'node';
  const sha = shortSha(GIT_SHA);
  const siteNew = updateAvailable(latest?.version, SITE_VERSION);
  const nodeNew = node?.known && updateAvailable(latest?.version, node.version);
  return (
    <div className="card">
      <h2>Versions</h2>
      <dl className="kv vers">
        <dt>Website</dt>
        <dd>
          {versionText(SITE_VERSION)}
          {sha && <> · <a href={commitUrl(GIT_SHA)} target="_blank" rel="noreferrer" className="mono">{sha}</a></>}
        </dd>
        <dt>Node</dt>
        <dd>
          <span className="mono">{url}</span> <span className="muted">({which})</span><br />
          {node ? (node.known ? versionText(node.version) : node.label) : 'no answer'}
          {chain && <span className="muted"> · {CHAIN_TEXT[chain]}</span>}
        </dd>
        {node?.gameBuild && <><dt>Game build</dt><dd className="mono">{node.gameBuild.slice(0, 12)}</dd></>}
        {node?.rules && <><dt>Rules</dt><dd>{node.rules}</dd></>}
        <dt>Latest</dt>
        <dd>
          {latest ? <a href={latest.url} target="_blank" rel="noreferrer">{latest.version}</a> : <span className="muted">unknown</span>}
          {siteNew && <b> · website update available</b>}
          {nodeNew && <b> · node update available</b>}
          {latest && !siteNew && !nodeNew && <span className="muted"> · up to date</span>}
        </dd>
      </dl>
    </div>
  );
}

export default function Info() {
  return (<>
    <Versions />
    <div className="card">
      <h2>Info</h2>

      <h3>How a block is won: the auction and the envelope</h3>
      <p>
        Each block is the next star, Bowser key or cap switch from where the last block ended, played with random controller pokes seeded by
        the block. Miners search for a run that gets there; the shorter the run, the better.
      </p>
      <p>
        <b>The time limit</b> (shown as &ldquo;accepting runs up to &hellip;&rdquo;) starts tight when the previous block lands
        and loosens about 17.5% per minute, until 20 minutes of play. A run becomes valid the moment the limit has grown to its
        length, so the <b>shortest run is valid first</b>. That is the auction.
      </p>
      <p>
        Miners keep improving privately and submit their best run a few seconds before it becomes valid. Within 30 seconds of
        the first valid block, a strictly shorter run from another miner still takes its place.
      </p>
      <p>
        Difficulty (retargeted to a 20-minute average) only shifts <i>when the curve starts</i>, not the rules.
      </p>
      <p>
        <b>The envelope graph</b> on the <Link to="/mine">Mine</Link> page and on every <Link to="/">block page</Link> draws
        that curve and puts each run on it as a dot. A dot under the curve is valid; the first dot the curve reaches wins.
      </p>

      <h3>Why are the controller inputs randomly poked?</h3>
      <p>
        Without the pokes, a run that worked once could be copied with tiny changes block after block. The random pokes,
        seeded by each block, mean every run has to be played anew: a recorded route falls apart, and only play that can
        recover from being knocked off course gets the star. Recovering is the sign of real skill, the same reason robotics
        researchers kick their walking robots.
      </p>

      <h3>How much of this is Monero?</h3>
      <p>
        All of the cryptography. SM64Coin is a fork of Monero v0.18.5.4: keys, signatures, ring signatures, hidden amounts
        and seed words are Monero's own code, unchanged, and the wallet on this site is Monero's wallet compiled to
        WebAssembly. What is new is how a block is won (playing Mario) and this website. See exactly what is which in{' '}
        <a href={`${REPO}/blob/main/docs/MONERO-FORK.md`} target="_blank" rel="noreferrer">MONERO-FORK.md</a>, or{' '}
        <a href="https://github.com/Gumbo64/sm64coin-monero/compare/v0.18.5.4...sm64coin" target="_blank" rel="noreferrer">every
        changed line against Monero</a>.
      </p>

      <h3>How do I run a node?</h3>
      <p>
        Download the bundle for your computer from the <a href={`${REPO}/releases/latest`} target="_blank" rel="noreferrer">latest
        release</a>, put your own US Super Mario 64 ROM next to it as <span className="mono">baserom.us.z64</span>, and open
        the wallet app: it starts its own node. On a server, run <span className="mono">./sm64coin-node</span> (a systemd unit is
        included) and open TCP port 16480.
      </p>

      <h3>Is there an Android app?</h3>
      <p>
        Yes: an APK (arm64, Android 7+) on the <a href={`${REPO}/releases/latest`} target="_blank" rel="noreferrer">release
        page</a>. It reads the chain from the public node and uses this website's wallet and your own ROM; you play to mine there (auto-mining in the app comes next). To install it and get updates
        automatically, <a href="https://apps.obtainium.imranr.dev/redirect?r=obtainium://add/https://github.com/Gumbo64/sm64coin" target="_blank" rel="noreferrer">add it to Obtainium</a>.
      </p>

      <h3>Can I create my own auto-miner?</h3>
      <p>
        Yes. Write your own mining bot with the Rust and Python SDKs: see the <a href={`${REPO}#writing-your-own-bot`} target="_blank" rel="noreferrer">README</a>.
        To run the existing auto-miner instead, use the <Link to="/mine">Mine</Link> page right here (the Auto-mine card) in the browser,
        press <b>Start</b> on the wallet app's Mining page, or run <span className="mono">sm64coin-automine</span> from the bundle.
      </p>
    </div>
  </>);
}
