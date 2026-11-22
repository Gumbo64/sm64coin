import React from 'react';
import { BrowserRouter, Routes, Route, NavLink } from 'react-router';
import Explorer from './pages/Explorer.jsx';
import Block from './pages/Block.jsx';
import Mine from './pages/Mine.jsx';
import Leaderboard from './pages/Leaderboard.jsx';
import Network from './pages/Network.jsx';
import Bots from './pages/Bots.jsx';
import NewBlockBanner from './lib/NewBlockBanner.jsx';

export default function App() {
  return (
    <BrowserRouter>
      <header className="nav">
        <span className="brand">★ SM64Coin</span>
        <NavLink to="/" end>Explorer</NavLink>
        <NavLink to="/mine">Mine</NavLink>
        <NavLink to="/leaderboard">Leaderboard</NavLink>
        <NavLink to="/bots">Build a bot</NavLink>
        <NavLink to="/network">Network</NavLink>
      </header>
      <NewBlockBanner />
      <main>
        <Routes>
          <Route path="/" element={<Explorer />} />
          <Route path="/block/:id" element={<Block />} />
          <Route path="/mine" element={<Mine />} />
          <Route path="/leaderboard" element={<Leaderboard />} />
          <Route path="/bots" element={<Bots />} />
          <Route path="/network" element={<Network />} />
        </Routes>
      </main>
    </BrowserRouter>
  );
}
