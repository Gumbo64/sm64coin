import React from 'react';
import { BrowserRouter, Routes, Route, NavLink, Navigate, useLocation } from 'react-router';
import Explorer from './pages/Explorer.jsx';
import Block from './pages/Block.jsx';
import Mine from './pages/Mine.jsx';
import Leaderboard from './pages/Leaderboard.jsx';
import Network from './pages/Network.jsx';
import Info from './pages/Info.jsx';
import NewBlockBanner from './lib/NewBlockBanner.jsx';
import UpdateBanner from './lib/UpdateBanner.jsx';
import LinkAddressBanner from './lib/LinkAddressBanner.jsx';
import LegacyBanner from './lib/LegacyBanner.jsx';
import MinerLight from './lib/MinerLight.jsx';
import { REPO } from './lib/versions.js';

// The wallet page is lazy: its engine (a wasm web worker) is fetched only when it is opened.
const Wallet = React.lazy(() => import('./pages/Wallet.jsx'));
const DesktopLink = React.lazy(() => import('./pages/DesktopLink.jsx')); // opened by the desktop app: hands the passkey wallet over

// The auto-miner and the manual miner are one page now: /automine (old links, the wallet's) lands on /mine, keeping ?address=.
function ToMine() {
  const { search, hash } = useLocation();
  return <Navigate to={`/mine${search}${hash}`} replace />;
}

// On every page. More links (the maintainer, socials) go in this list.
function SiteFooter() {
  return (
    <footer className="site-footer">
      <span>
        Wallet and all non-proof-of-work code forked from <a href="https://github.com/monero-project/monero" target="_blank" rel="noreferrer">Monero</a>{' '}
        (<a href={`${REPO}/blob/main/docs/MONERO-FORK.md`} target="_blank" rel="noreferrer">what changed</a>)
      </span>
      <span><a href={REPO} target="_blank" rel="noreferrer">Source</a></span>
    </footer>
  );
}

export default function App() {
  return (
    <BrowserRouter basename={import.meta.env.BASE_URL.replace(/\/$/, '')}>
      <header className="nav">
        <span className="brand">★ SM64Coin</span>
        <nav className="tabs" aria-label="Pages">
          <NavLink to="/" end>Explorer</NavLink>
          <NavLink to="/mine">Mine</NavLink>
          <NavLink to="/wallet">Wallet</NavLink>
          <NavLink to="/leaderboard">Leaderboard</NavLink>
          <NavLink to="/network">Network</NavLink>
          <NavLink to="/info">Info</NavLink>
        </nav>
        <MinerLight />
      </header>
      <LegacyBanner />
      <LinkAddressBanner />
      <UpdateBanner />
      <NewBlockBanner />
      <main>
        <Routes>
          <Route path="/" element={<Explorer />} />
          <Route path="/block/:id" element={<Block />} />
          <Route path="/mine" element={<Mine />} />
          <Route path="/automine" element={<ToMine />} />
          <Route path="/wallet" element={<React.Suspense fallback={<p className="muted">Loading…</p>}><Wallet /></React.Suspense>} />
          <Route path="/desktop-link" element={<React.Suspense fallback={<p className="muted">Loading…</p>}><DesktopLink /></React.Suspense>} />
          <Route path="/leaderboard" element={<Leaderboard />} />
          <Route path="/info" element={<Info />} />
          <Route path="/network" element={<Network />} />
        </Routes>
      </main>
      <SiteFooter />
    </BrowserRouter>
  );
}
