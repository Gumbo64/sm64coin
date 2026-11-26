import React from 'react';
import { Link } from 'react-router';
import { DEDICATED, NEW_SITE_URL } from './security.js';

// Only on the legacy GitHub Pages build: the app moved to its own origin (docs/WEB-SECURITY.md); this site stays for moving wallets.
export default function LegacyBanner() {
  if (DEDICATED) return null;
  return (
    <div className="update-banner" data-testid="legacy-banner">
      SM64Coin has moved to <a href={NEW_SITE_URL} rel="noopener">{NEW_SITE_URL.replace(/^https:\/\//, '')}</a>. This address is kept for moving your wallet:
      <Link to="/wallet"> Wallet, "Move to the new site"</Link>.
    </div>
  );
}
