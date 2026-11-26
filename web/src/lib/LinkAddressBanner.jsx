import React from 'react';
import { miner } from './webminer/controller.js';
import { useMiner } from './webminer/useMiner.js';

// A link such as /mine?address=SM6... is never applied silently: whoever made the link would receive the mining rewards.
export default function LinkAddressBanner() {
  const s = useMiner();
  const a = s.linkAddress;
  if (!a) return null;
  return (
    <div className="update-banner required" role="alert" data-testid="link-address-banner">
      This link wants to set your <b>mining payout address</b> to <span className="mono break">{a}</span>
      {s.address ? <> (now: <span className="mono">{s.address.slice(0, 10)}…{s.address.slice(-6)}</span>)</> : null}.
      Rewards go there. Only accept it if it is your own address.{' '}
      <button onClick={() => miner.acceptLinkAddress()}>Use this address</button>{' '}
      <button onClick={() => miner.dismissLinkAddress()}>Ignore</button>
    </div>
  );
}
