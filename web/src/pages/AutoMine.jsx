import React, { useEffect } from 'react';
import RomGate from '../lib/RomGate.jsx';
import CycleTimeline from '../lib/CycleTimeline.jsx';
import { miner, isAddress } from '../lib/webminer/controller.js';
import { useMiner } from '../lib/webminer/useMiner.js';
import { Cols, Row, InfoPop } from '../components/ui.jsx';
import './automine/automine.css';
import ControlCard from './automine/ControlCard.jsx';
import CampaignStrips from './automine/CampaignStrips.jsx';

// The automated miner in the browser: the native miner's RRT campaign (objective-RRT search,
// pow/webminer) compiled to WebAssembly, one campaign per Web Worker. Each worker searches the
// same block template with its own salt (as `automine --salt`); the first run found is sealed,
// stamped when it is valid and submitted. A new tip restarts them on the new template.
// This page is only a view: the miner itself (lib/webminer/controller.js) lives as long as the site's
// page, so it goes on while you visit other pages (the header's light shows what it is doing).
// Layout (as the Mine page): the cycle strip, the main column (live game, search cloud) and a slim sidebar
// (the Auto-mine card, the limit chart, the block's numbers, progress, blocks mined); workers and log are
// collapsed rows below. The visualisations are loaded only while switched on (the toggle is remembered);
// off, workers are told to stop sampling their trees.
const VizMain = React.lazy(() => import('./automine/VizPanel.jsx').then((m) => ({ default: m.VizMain })));
const VizSide = React.lazy(() => import('./automine/VizPanel.jsx').then((m) => ({ default: m.VizSide })));

export default function AutoMine() {
  return (
    <RomGate fallback={<div className="card"><h2>Auto-mine</h2><p>The automated miner plays the game in this browser: it needs your US Super Mario 64 ROM (it stays in this browser).</p></div>}>
      <AutoMiner />
    </RomGate>
  );
}

function AutoMiner() {
  const s = useMiner();
  const { running, log, workers, mode, vizOn } = s;
  // The views redraw only while this page is on screen.
  useEffect(() => {
    const q = new URLSearchParams(window.location.search);
    const a = q.get('address');
    if (a && isAddress(a.trim())) miner.setAddress(a.trim());
    if (q.get('vizmock') === '1' && !miner.running) miner.startMock({ count: 3, runAfter: Number(q.get('run') || 0) * 1000 });
    miner.setViewing(true);
    return () => miner.setViewing(false);
  }, []);
  const hub = miner.hub;
  const fb = <div className="viz-card"><p className="muted">Loading visualisations…</p></div>;
  return (
    <>
      <CycleTimeline attempt={0} limit={hub.template ? hub.template.sm64_max_frames : 0} />
      <p className="ui-oneline">
        Your browser plays Mario 64 for you and mines blocks. It uses a lot of CPU while it runs.
        <InfoPop>
          The automated miner in your browser: the same campaign search as the native <code>sm64coin-automine</code> (objective-RRT, compiled to WebAssembly), one per worker, each with its own seed.
          It keeps running while you visit other pages of the site (stop it here). Rewards go to your address; nothing leaves this browser but the block.
        </InfoPop>
      </p>
      <Cols
        main={vizOn
          ? <React.Suspense fallback={fb}><VizMain hub={hub} /></React.Suspense>
          : <div className="viz-card"><p className="muted">The visualisations are off: all CPU goes to mining. Switch them on in the Auto-mine card's ⚙ to watch the game and the search.</p></div>}
        side={<>
          <ControlCard s={s} />
          {vizOn && <React.Suspense fallback={fb}><VizSide hub={hub} mode={mode} /></React.Suspense>}
        </>}
      />
      <Row title="Workers" count={workers.length || undefined} storeKey="workers">
        {workers.length > 0 && (
          <table className="small">
            <tbody>
              {workers.map((w, i) => <tr key={i}><td>worker {i + 1}</td><td className="muted">salt {w.salt}{w.runs ? ` · ${w.runs} search${w.runs === 1 ? '' : 'es'} done` : ''}</td><td className="mono">{w.line}</td></tr>)}
            </tbody>
          </table>
        )}
        {vizOn ? <CampaignStrips hub={hub} /> : !workers.length && <p className="muted small">No workers running.</p>}
      </Row>
      <Row title="Log" storeKey="log"><pre className="log small">{log.join('\n') || (running ? '' : 'Nothing yet.')}</pre></Row>
    </>
  );
}
