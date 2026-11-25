import React, { useState } from 'react';
import './ui.css';
import '../pages/automine/automine.css'; // the panel/settings/gear styles (viz-head, viz-settings, ...) are shared

// Small layout pieces shared by the pages (Mine, Auto-mine, and the merged hybrid page later):
//  Toolbar / ToolBtn   a labelled row of buttons (one group per concern: playback, speed, actions)
//  Panel               a main-column panel: title row with a ⚙ at the right, settings form when open, then the content
//  SideCard            a card of the slim right column (title row, optional tools at the right)
//  Row                 a collapsed one-line row ("▸ Workers (2)") that opens in place; open/closed is remembered
//  InfoPop             an ⓘ that opens a short explanation
//  Cols                the two-column layout: main on the left, a slim sidebar on the right; stacked on phones
//  useStored           useState remembered in localStorage (never throws)

export function useStored(key, def) {
  const [v, set] = useState(() => { try { const s = localStorage.getItem(key); return s == null ? def : JSON.parse(s); } catch { return def; } });
  const put = (x) => set((o) => { const n = typeof x === 'function' ? x(o) : x; try { localStorage.setItem(key, JSON.stringify(n)); } catch { /* private window */ } return n; });
  return [v, put];
}

export function Toolbar({ label, children, className = '' }) {
  return <div className={`ui-toolbar ${className}`} role="toolbar" aria-label={label}>{children}</div>;
}

export function ToolBtn({ on, children, className = '', ...p }) {
  return <button type="button" className={`ui-btn${on ? ' on' : ''} ${className}`} {...p}>{children}</button>;
}

export function GearBtn({ open, onClick, label = 'Settings' }) {
  return <button type="button" className="viz-gear" aria-expanded={!!open} aria-label={label} title={label} onClick={onClick}>⚙</button>;
}

export function Panel({ title, chip, open, onToggle, settings, children, className = '', innerRef }) {
  return (
    <div className={`viz-card ui-panel ${className}`} ref={innerRef}>
      <div className="viz-head">
        <h3>{title} {chip}</h3>
        {onToggle && <div className="viz-tools"><GearBtn open={open} onClick={onToggle} /></div>}
      </div>
      {open && settings}
      {children}
    </div>
  );
}

export function SideCard({ title, tools, children, className = '' }) {
  return (
    <section className={`card ui-side ${className}`}>
      {(title || tools) && <div className="viz-head"><h3>{title}</h3>{tools && <div className="viz-tools">{tools}</div>}</div>}
      {children}
    </section>
  );
}

export function Row({ title, storeKey, children, count }) {
  const [open, setOpen] = useStored(`sm64coin.row.${storeKey}`, false);
  return (
    <div className="ui-row">
      <button type="button" className="ui-row-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span className="tri">{open ? '▾' : '▸'}</span> {title}{count != null ? ` (${count})` : ''}
      </button>
      {open && <div className="ui-row-body">{children}</div>}
    </div>
  );
}

export function InfoPop({ children, label = 'About this page' }) {
  const [open, setOpen] = useState(false);
  return (
    <span className="ui-info">
      <button type="button" className="viz-gear" aria-expanded={open} aria-label={label} title={label} onClick={() => setOpen(!open)}>ⓘ</button>
      {open && <span className="ui-info-pop" role="note">{children}</span>}
    </span>
  );
}

export function Cols({ main, side, className = '' }) {
  return (
    <div className={`mine2 ui-cols ${className}`}>
      <div className="ui-main">{main}</div>
      <aside className="ui-sidecol">{side}</aside>
    </div>
  );
}
