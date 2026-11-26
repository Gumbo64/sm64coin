// Is a node on this chain? SM64Coin v0.3 is a new chain (new genesis), so a node still running v0.2 answers
// every call happily but shows the old chain. The genesis block's hash tells them apart.
// VITE_GENESIS overrides it for other networks (testnet builds); set it to 'any' to skip the check.
const MAINNET_GENESIS = 'cbd56c9bf4ea17392ccfdb7a661f0e3e449b55b95a9a2e1eeea426132dfbde9f';
export const GENESIS = (import.meta.env?.VITE_GENESIS || MAINNET_GENESIS).trim().toLowerCase();

/**
 * `call(method, params)` -> result (the node's JSON-RPC). Returns
 * { state: 'ok' | 'wrong-chain' | 'down', genesis? }. A node that does not answer is 'down'; one whose block 0 is
 * not ours is 'wrong-chain'.
 */
export async function probeChain(call, genesis = GENESIS) {
  let hash;
  try {
    const r = await call('get_block_header_by_height', { height: 0 });
    hash = String(r?.block_header?.hash || '').toLowerCase();
  } catch {
    return { state: 'down' };
  }
  if (!hash) return { state: 'down' };
  if (genesis === 'any' || hash === genesis) return { state: 'ok', genesis: hash };
  return { state: 'wrong-chain', genesis: hash };
}

/** The visible note for a visit; '' when there is nothing to say. */
export function chainNote({ chosen, node, fallbackFrom, probe, publicNode }) {
  if (fallbackFrom === 'wrong-chain') {
    return `Your local node is on an old chain (v0.2): using ${publicNode}. Update your node (SM64Coin 0.3 or newer) to use it.`;
  }
  if (chosen && probe === 'wrong-chain') {
    return `The node you chose (${node}) is on a different chain (an old v0.2 node?), not SM64Coin v0.3: what you see is not this chain. Update that node or pick another.`;
  }
  return '';
}
