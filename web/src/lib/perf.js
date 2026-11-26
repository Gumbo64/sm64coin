// Start-up timing marks (performance.mark "sm64:<name>"): Start -> first search / first cloud points / live game.
// `at` is an absolute epoch ms (a worker's mark, see worker.js), else now. window.__sm64marks() (?e2e) lists them.
let marks = [];
let seen = new Set();
export function mark(name, at) {
  try {
    if (name === 'start-pressed') { marks = []; seen = new Set(); }
    const t = at == null ? performance.now() : at - performance.timeOrigin;
    performance.mark(`sm64:${name}`, { startTime: t });
    marks.push([name, Math.round(t)]);
  } catch { /* no performance API */ }
}
/** mark(), the first time since Start only. */
export function markOnce(name) { if (!seen.has(name)) { seen.add(name); mark(name); } }
export const allMarks = () => marks.slice();
