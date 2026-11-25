import React from 'react';
import { NavLink } from 'react-router';
import './minerlight.css';
import { useMiner } from './webminer/useMiner.js';

// The mining status light in the header, on every page: its background is yellow when the auto-miner is off and
// green when it is on; the finer state is the icon and text (pulsing dot: mining, spinner: preparing, diamond: run
// found / waiting / submitting, star: block accepted, cross: error or rejected, loop: reset-cycle attempt). Always with a short text (and an icon), never colour alone. Click: the Auto-mine page.
const ICON = { idle: '○', loading: '◌', mining: '●', found: '◆', accepted: '★', error: '✕', reset: '↻' };

export default function MinerLight() {
  const { status } = useMiner();
  return (
    <NavLink to="/mine" className={`minerlight ${status.on ? 'on' : 'off'} ${status.kind}`} title={status.title} aria-label={`Auto-miner: ${status.title}`} role="status">
      <span className="ml-ico" aria-hidden="true">{ICON[status.kind]}</span>
      <span className="ml-txt">Auto-mine: {status.label}</span>
    </NavLink>
  );
}
