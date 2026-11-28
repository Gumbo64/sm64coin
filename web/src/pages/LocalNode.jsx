import React, { useEffect, useState } from 'react';
import { getPlatform } from '../platform/index.js';
import { nodeInUse } from '../lib/rpc.js';
import useInterval from '../lib/useInterval.js';
import { Row, Toolbar, ToolBtn } from '../components/ui.jsx';
import { asPort, effectivePublicPort, portHint, validListenPort, validPublicPort } from '../lib/nodeports.js';

// The app's own full node: its state, the P2P port a VPN's forward must reach, start/stop and the log tails. Desktop only
// (platform.node is null in a browser tab and on Android); rendered at the top of the Network page.

const clean = (d) => ({ p2p_port: asPort(d.p2p_port), p2p_external_port: asPort(d.p2p_external_port), auto_start: !!d.auto_start });
const same = (a, b) => !!a && !!b && a.p2p_port === b.p2p_port && a.p2p_external_port === b.p2p_external_port && a.auto_start === b.auto_start;

function Dot({ state }) {
  return <span className={`node-dot ${state}`} aria-hidden="true" />;
}

function Logs() {
  const p = getPlatform();
  const [logs, setLogs] = useState(null);
  const [tab, setTab] = useState('daemon');
  useInterval(() => { p.node.log(160).then(setLogs).catch(() => {}); }, 3000);
  const text = logs ? logs[tab] : '';
  return (
    <>
      <div className="controls">
        <button type="button" className={tab === 'daemon' ? 'on' : ''} onClick={() => setTab('daemon')}>Daemon log</button>
        <button type="button" className={tab === 'app' ? 'on' : ''} onClick={() => setTab('app')}>Startup log</button>
        {logs && <span className="muted mono small">{logs.dir}</span>}
      </div>
      <pre className="log small">{text || `No ${tab === 'daemon' ? 'daemon' : 'startup'} log yet.`}</pre>
    </>
  );
}

export default function LocalNode() {
  const p = getPlatform();
  const [host, setHost] = useState(null);
  const [info, setInfo] = useState(null);
  const [settings, setSettings] = useState(null);
  const [draft, setDraft] = useState(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState('');
  const [note, setNote] = useState('');
  const [detected, setDetected] = useState(null);

  useEffect(() => {
    if (!p.node) return;
    p.host().then(setHost).catch(() => {});
    p.node.settings().then((s) => { setSettings(s); setDraft(s); }).catch((e) => setErr(e.message));
  }, [p]);
  useInterval(() => { if (p.node) p.node.status().then(setInfo).catch(() => {}); }, 2000);

  if (!p.node) return null;
  if (!draft || !settings) {
    return <section className="card"><h2>Local node</h2><p className="muted">Checking the node settings…</p>{err && <p className="error small">{err}</p>}</section>;
  }

  const use = nodeInUse();
  const kind = use.kind === 'chosen' ? 'chosen' : info && info.answering ? 'local' : use.kind;
  const startingNow = !!(info && (info.starting || (info.running && !info.answering)));
  const useText = kind === 'public' && startingNow
    ? 'The local node is starting; the app reads from the public node meanwhile.'
    : {
        local: 'This app is reading from the local node.',
        public: 'The local node is not answering, so the app reads from the public node.',
        chosen: 'This app is reading from the remote node chosen below; the local node is not used.',
        none: 'No node is answering.',
      }[kind] || '';
  const starting = !!(info && info.starting);
  const running = !!(info && (info.running || info.reused));
  const answering = !!(info && info.answering);
  const state = !info ? 'checking' : starting ? 'starting' : answering ? (info.synchronized ? 'ok' : 'sync') : info.error ? 'bad' : running ? 'loading' : 'off';
  const stateText = !info
    ? 'Checking the node…'
    : starting
      ? 'Starting the node…'
      : answering
        ? (info.synchronized ? `Synced at block ${info.height}` : `Syncing: block ${info.height} of ${info.target_height || '?'}`)
        : info.error
          ? `Stopped: ${String(info.error).split('\n')[0]}`
          : info.reused
            ? 'An existing node is in use but is not answering'
            : running
              ? 'Running, waiting for the RPC…'
              : 'Stopped';
  const p2p = asPort(info?.p2p_port) || asPort(settings.p2p_port);
  const publicPort = effectivePublicPort({ p2p_port: p2p, p2p_external_port: info?.p2p_external_port || settings.p2p_external_port });
  const romOk = !host || (host.rom && host.rom.found);
  const nodeBinOk = !host || host.has_node_binary;
  const changed = !same(clean(draft), settings);
  const hint = portHint(info);

  const run = async (f) => {
    setBusy(true);
    setErr('');
    try {
      await f();
      setInfo(await p.node.status());
    } catch (e) {
      setErr(e.message);
    } finally {
      setBusy(false);
    }
  };

  const save = (restart) => run(async () => {
    const s = clean(draft);
    if (!validListenPort(s.p2p_port)) throw new Error('The P2P port must be 1024 or higher.');
    if (!validPublicPort(s.p2p_external_port)) throw new Error('The public port must be 0 (same as P2P) or 1024 or higher.');
    await p.node.setSettings(s);
    setSettings(s);
    if (restart) {
      await p.node.stop().catch(() => {});
      await p.node.start();
      setNote('Saved and restarted with the new ports.');
    } else {
      setNote(!running ? 'Saved.' : info?.reused ? 'Saved. Stop the existing node where it was started to apply the ports.' : 'Saved. Restart the node to apply the ports.');
    }
  });

  const detect = () => run(async () => {
    const port = asPort(draft.p2p_port);
    if (!validListenPort(port)) throw new Error('Set a valid P2P port (1024 or higher) first.');
    const m = await p.node.detectPort(port);
    setDetected(m);
    setDraft((d) => ({ ...d, p2p_external_port: m.external_port }));
    setNote(`The gateway says its public port is ${m.external_port} (${m.method || 'port mapping'}, via ${m.gateway}). Save it to use it.`);
  });

  const pickRom = () => run(async () => {
    const r = await p.rom.pick();
    setHost(await p.host());
    if (r) setNote('ROM installed.');
  });

  return (
    <section className="card" id="local-node">
      <h2>Local node</h2>
      <p className="muted">
        A full SM64Coin node run by this app: it verifies every block and is what the app reads from and mines against while it
        answers. The app {settings.auto_start ? 'starts it when it opens' : 'leaves it stopped until you start it'}.
      </p>
      <p><Dot state={state} /> <b>{stateText}</b> <span className="muted">| {useText}</span></p>
      {kind === 'public' && use.url && <p className="muted">Reading from: <code>{use.url}</code></p>}
      {hint && <p className="notice">{hint}</p>}
      {!nodeBinOk && <p className="notice">The app is missing <span className="mono">sm64coind</span>: reinstall the app or point <span className="mono">SM64COIN_BUNDLE</span> at the release folder.</p>}
      {!romOk && (
        <div className="notice">
          <p>The node needs your US Super Mario 64 ROM. It is checked (SHA-1) and copied into the app's data folder; it is not included.</p>
          <Toolbar label="ROM"><ToolBtn disabled={busy} onClick={pickRom}>Choose ROM file…</ToolBtn></Toolbar>
        </div>
      )}

      <div className="node-ports">
        <label htmlFor="node-p2p">P2P port (listen)</label>
        <input id="node-p2p" type="number" min="1024" max="65535" value={draft.p2p_port} disabled={busy}
          onChange={(e) => { setDetected(null); setDraft((d) => ({ ...d, p2p_port: e.target.value })); }} />
        <label htmlFor="node-public">Public port (VPN forward)</label>
        <input id="node-public" type="number" min="0" max="65535" value={draft.p2p_external_port || ''} disabled={busy}
          placeholder={`same as P2P (${asPort(draft.p2p_port) || '?'})`}
          onChange={(e) => setDraft((d) => ({ ...d, p2p_external_port: e.target.value }))} />
        <ToolBtn disabled={busy} onClick={detect} title="Ask the VPN/router gateway for the public port it forwards to the P2P port (NAT-PMP, PCP or UPnP-IGD)">Detect from VPN</ToolBtn>
      </div>
      <p className="muted small">
        A VPN's port forward must reach this machine's P2P port. If it forwards public port X to the same local port, set both
        fields to X. If it forwards X to a different local port, keep P2P at the local port and set Public port to X: peers are
        told the public one. If your VPN's gateway implements NAT-PMP, PCP or UPnP-IGD, <b>Detect from VPN</b> asks it for the
        public port; if it does not, enter the port shown by your VPN app. The port must be free; the app will not silently
        move it.
      </p>
      <label className="checkline"><input type="checkbox" checked={!!draft.auto_start} disabled={busy} onChange={(e) => setDraft((d) => ({ ...d, auto_start: e.target.checked }))} /> Start the node when the app opens</label>

      <Toolbar label="Local node">
        {!running && !starting && <ToolBtn disabled={busy || !romOk || !nodeBinOk} onClick={() => run(() => p.node.start())}>Start node</ToolBtn>}
        {running && !starting && !info?.reused && <ToolBtn disabled={busy} onClick={() => run(() => p.node.stop())}>Stop node</ToolBtn>}
        {changed && <ToolBtn disabled={busy} onClick={() => save(false)}>Save</ToolBtn>}
        {running && !starting && !info?.reused && changed && <ToolBtn disabled={busy} onClick={() => save(true)}>Save and restart</ToolBtn>}
      </Toolbar>
      {info?.reused && <p className="muted small">A node was already running on this machine; the app is using it and leaves it running when it quits. Stop it where it was started.</p>}
      {note && <p className="muted small">{note}</p>}
      {err && <p className="error small">{err}</p>}

      <dl className="kv">
        <dt>RPC</dt><dd><code>http://127.0.0.1:{asPort(info?.rpc_port) || 16481}</code> <span className="muted">(local only)</span></dd>
        <dt>P2P</dt><dd><code>{p2p || '?'}</code> local · <code>{publicPort || '?'}</code> public {info?.p2p_external_port ? '(forwarded)' : ''}</dd>
        <dt>Peers</dt><dd>{answering ? `${info.incoming_peers || 0} incoming, ${info.outgoing_peers || 0} outgoing` : '—'}</dd>
        <dt>ZMQ</dt><dd><code>{info?.zmq_port || '?'}</code> <span className="muted">(local only)</span></dd>
        <dt>Data</dt><dd className="mono break">{host?.data_dir || '—'}{host?.portable ? ' (portable)' : ''}</dd>
        <dt>ROM</dt><dd>{host?.rom?.found ? <span className="mono break">{host.rom.path}</span> : 'not found'}</dd>
      </dl>
      {detected && <p className="muted small">{detected.method || 'Port mapping'}: {detected.gateway} forwards public port {detected.external_port} to local port {detected.internal_port} (renews automatically while the app runs).</p>}

      <Row title="Node log" storeKey="network-node-log"><Logs /></Row>
    </section>
  );
}
