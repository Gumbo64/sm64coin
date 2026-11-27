import React from 'react';
import { Link } from 'react-router';
import { isAddress } from './webminer/controller.js';
import Identicon from '../components/Identicon.jsx';
import { groupedShort } from './identicon.js';

/** An address: its identicon and a grouped short form; "ACTIVE WALLET" (accent) when it is this device's wallet. */
export function AddrLabel({ address, wallet, size = 18 }) {
  if (!isAddress(address || '')) return <em>no address</em>;
  const mine = wallet && address === wallet;
  return (
    <span style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }} title={address}>
      <Identicon address={address} size={size} />
      {mine ? <b style={{ color: 'var(--acc)' }}>ACTIVE WALLET</b> : <span className="mono">{groupedShort(address)}</span>}
    </span>
  );
}

/** Shown when there is no mining address. */
export function NoWallet({ address }) {
  if (isAddress(address || '')) return null;
  return <div className="notice warn" data-testid="no-wallet" role="status">No mining address. <Link to="/wallet">Open your wallet</Link> or paste an address.</div>;
}
