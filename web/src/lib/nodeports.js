// The local node's port settings and the "is my VPN forward working?" hint. Pure functions: the Local node page and its test
// share them. The backend validates again; this is the friendly side.

export const asPort = (v, fallback = 0) => {
  const n = Number(v);
  return Number.isInteger(n) && n >= 0 && n <= 65535 ? n : fallback;
};

/** A listening P2P port: 1024+ (below that the daemon needs administrator rights). */
export const validListenPort = (v) => asPort(v) >= 1024;

/** 0 means "same as the listening port"; otherwise a public port can be anything the VPN assigned (even below 1024). */
export const validPublicPort = (v) => {
  const n = asPort(v, -1);
  return n >= 0 && n <= 65535;
};

/** The port peers are told: the configured public port, else the listening port. */
export function effectivePublicPort(settings) {
  const s = settings || {};
  return asPort(s.p2p_external_port) || asPort(s.p2p_port);
}

/** What the user should do when the node has outgoing peers but none can reach it. Null while that is not the case. */
export function portHint(status) {
  if (!status || !status.answering || Number(status.outgoing_peers || 0) === 0 || Number(status.incoming_peers || 0) > 0) return null;
  const local = asPort(status.p2p_port);
  const pub = asPort(status.p2p_external_port) || local;
  return `No incoming peers yet: if you use a VPN or router port forward, make sure public port ${pub} reaches this machine's port ${local}.`;
}
