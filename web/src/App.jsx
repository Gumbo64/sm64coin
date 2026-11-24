import React from 'react';
import { BrowserRouter, Routes, Route, NavLink } from 'react-router';
import Explorer from './pages/Explorer.jsx';
import Block from './pages/Block.jsx';
import Mine from './pages/Mine.jsx';
import AutoMine from './pages/AutoMine.jsx';
import Leaderboard from './pages/Leaderboard.jsx';
import Network from './pages/Network.jsx';
import Info from './pages/Info.jsx';
import NewBlockBanner from './lib/NewBlockBanner.jsx';
import UpdateBanner from './lib/UpdateBanner.jsx';
import MinerLight from './lib/MinerLight.jsx';

// The wallet page is lazy: its engine (a wasm web worker) is fetched only when it is opened.
const Wallet = React.lazy(() => import('./pages/Wallet.jsx'));

export default function App() {
  return (
    <BrowserRouter basename={import.meta.env.BASE_URL.replace(/\/$/, '')}>
      <header className="nav">
        <span className="brand">★ SM64Coin</span>
        <NavLink to="/" end>Explorer</NavLink>
        <NavLink to="/mine">Mine</NavLink>
        <NavLink to="/wallet">Wallet</NavLink>
        <NavLink to="/leaderboard">Leaderboard</NavLink>
        <NavLink to="/network">Network</NavLink>
        <NavLink to="/info">Info</NavLink>
        <MinerLight />
      </header>
      <UpdateBanner />
      <NewBlockBanner />
      <main>
        <Routes>
          <Route path="/" element={<Explorer />} />
          <Route path="/block/:id" element={<Block />} />
          <Route path="/mine" element={<Mine />} />
          <Route path="/automine" element={<AutoMine />} />
          <Route path="/wallet" element={<React.Suspense fallback={<p className="muted">Loading…</p>}><Wallet /></React.Suspense>} />
          <Route path="/leaderboard" element={<Leaderboard />} />
          <Route path="/info" element={<Info />} />
          <Route path="/network" element={<Network />} />
        </Routes>
      </main>
    </BrowserRouter>
  );
}
