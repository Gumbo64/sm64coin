import React, { useState } from 'react';
import { useStored } from '../components/ui.jsx';
import { asPort, effectivePublicPort, validListenPort, validPublicPort } from '../lib/nodeports.js';

// The website cannot start or reconfigure a node it does not run. This card keeps the two values a VPN port forward
// needs and builds the exact daemon flags, so a browser user can set them on their own node (the desktop app's Local
// node card applies them directly). Shown only in a browser tab, not in the apps.

export default function NodePortsHelp() {
  const [p2p, setP2p] = useStored('sm64coin.nodeP2pPort', 16480);
  const [pub, setPub] = useStored('sm64coin.nodePublicPort', 0);
  const [copied, setCopied] = useState(false);
  const listen = asPort(p2p);
  const publicPort = effectivePublicPort({ p2p_port: listen, p2p_external_port: asPort(pub) });
  const ok = validListenPort(listen) && validPublicPort(asPort(pub));
  const external = asPort(pub) && asPort(pub) !== listen ? ` --p2p-external-port ${asPort(pub)}` : '';
  const command = ok ? `sm64coind --p2p-bind-port ${listen}${external}` : '';
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch { /* clipboard unavailable */ }
  };

  return (
    <section className="card" id="node-ports">
      <h2>Your node's ports</h2>
      <p className="muted">
        This website does not run a node, so it cannot change one. If you run your own node, set its P2P port to the local
        port your VPN/router forwards, and its public port to the forwarded one. The desktop app does this directly on
        <b> Network → Local node</b>, where <b>Detect from VPN</b> tries NAT-PMP, PCP and UPnP-IGD.
      </p>
      <div className="node-ports">
        <label htmlFor="web-p2p">P2P port (listen)</label>
        <input id="web-p2p" type="number" min="1024" max="65535" value={p2p} onChange={(e) => setP2p(e.target.value)} />
        <label htmlFor="web-public">Public port (VPN forward)</label>
        <input id="web-public" type="number" min="0" max="65535" value={pub || ''} placeholder={`same as P2P (${listen || '?'})`}
          onChange={(e) => setPub(e.target.value)} />
      </div>
      {!ok && <p className="error small">The P2P port must be 1024 or higher; the public port is 0 (same as P2P) or 1024 or higher.</p>}
      {ok && (
        <>
          <p className="muted small">Start your node with these flags (add its ROM, data dir and usual options):</p>
          <div className="controls">
            <code className="mono break">{command}</code>{' '}
            <button type="button" onClick={copy}>{copied ? 'Copied' : 'Copy'}</button>
          </div>
          <p className="muted small">
            Peers are told the public port {publicPort}; the VPN must forward it to this machine's {listen}. If the VPN's
            gateway does not expose a port automatically, the port shown by the VPN app is the public one.
          </p>
        </>
      )}
    </section>
  );
}
