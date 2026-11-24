import { useEffect, useState } from 'react';

/** Re-renders the component when the hub's data changed (at most 4 times a second). */
export function useHub(hub) {
  const [v, setV] = useState(0);
  useEffect(() => hub.subscribe(setV), [hub]);
  return v;
}

/** Whether `ref`'s element is on screen and the tab is in the foreground (views pause otherwise). */
export function useVisible(ref) {
  const [vis, setVis] = useState(true);
  useEffect(() => {
    const el = ref.current;
    if (!el || typeof IntersectionObserver === 'undefined') return undefined;
    let inView = true, fg = !document.hidden;
    const upd = () => setVis(inView && fg);
    const io = new IntersectionObserver((es) => { inView = es[es.length - 1].isIntersecting; upd(); });
    io.observe(el);
    const onVis = () => { fg = !document.hidden; upd(); };
    document.addEventListener('visibilitychange', onVis);
    return () => { io.disconnect(); document.removeEventListener('visibilitychange', onVis); };
  }, [ref]);
  return vis;
}
