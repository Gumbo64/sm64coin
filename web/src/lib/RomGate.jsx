import React, { useEffect, useState } from 'react';
import { FileUpload } from 'sm64-binds-frontend';
import { getPlatform } from '../platform/index.js';

// The game needs a US ROM, cached in the browser after the first upload.
// `fallback`: shown above the upload prompt while there is no ROM.
// In the desktop app the ROM is the one the app keeps (chosen once with a file dialog): the page copies it into its own cache
// instead of asking for a second upload.
export default function RomGate({ children, fallback = null }) {
  const [hasRom, setHasRom] = useState(false);
  const rom = getPlatform().rom;
  if (hasRom) return children;
  if (rom) return <>{fallback}<DesktopRom rom={rom} quiet={!!fallback} onReady={() => setHasRom(true)} /></>;
  return <>{fallback}<div className="card romgate"><FileUpload setHasRom={setHasRom} /></div></>;
}

function DesktopRom({ rom, quiet, onReady }) {
  const [state, setState] = useState({ phase: 'checking', error: '' }); // checking | need | loading
  const feed = async () => {
    setState({ phase: 'loading', error: '' });
    try {
      const f = await import('sm64-binds-src/scripts/fileUpload.js');
      if (await f.isRomCached().then(() => true, () => false)) await f.updateWASMs();
      else await f.storeROM(await rom.bytes());
      onReady();
    } catch (e) { setState({ phase: 'need', error: String((e && e.message) || e) }); }
  };
  useEffect(() => {
    let dead = false;
    rom.status().then((s) => { if (dead) return; if (s.found) feed(); else setState({ phase: 'need', error: '' }); },
      (e) => !dead && setState({ phase: 'need', error: String(e.message || e) }));
    const again = () => rom.status().then((s) => s.found && feed(), () => {});
    window.addEventListener('sm64coin:rom', again);
    return () => { dead = true; window.removeEventListener('sm64coin:rom', again); };
  }, []);
  const pick = async () => {
    try {
      const r = await rom.pick();
      if (r && r.found) feed();
    } catch (e) { setState({ phase: 'need', error: e.message }); }
  };
  if (state.phase === 'need' && quiet && !state.error) return null; // the fallback (Auto-mine card) already asks for the ROM
  if (state.phase !== 'need') return <div className="card romgate"><p>{state.phase === 'loading' ? 'Loading the game...' : 'Looking for your ROM...'}</p></div>;
  return (
    <div className="card romgate">
      <h2>Super Mario 64 (US) ROM</h2>
      <p>The app needs your own US ROM (.z64, 8 MB) once; it is kept in the app's data folder for the game and the node.</p>
      <button className="btn" onClick={pick}>Choose ROM file...</button>
      {state.error && <p style={{ color: 'red' }}>{state.error}</p>}
    </div>
  );
}
