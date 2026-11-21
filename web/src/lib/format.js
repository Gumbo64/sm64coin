export const ATOMIC = 1e12;
export const coins = (atomic) => (Number(atomic) / ATOMIC).toLocaleString(undefined, { maximumFractionDigits: 6 });
export const short = (h, n = 10) => (h ? `${h.slice(0, n)}…` : '');
export const frames = (f) => {
  const s = f / 30;
  return `${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, '0')}`;
};
export function ago(ts) {
  const d = Math.max(0, Date.now() / 1000 - ts);
  if (d < 60) return `${Math.floor(d)} s ago`;
  if (d < 3600) return `${Math.floor(d / 60)} min ago`;
  if (d < 86400) return `${Math.floor(d / 3600)} h ago`;
  return `${Math.floor(d / 86400)} d ago`;
}
export const when = (ts) => new Date(ts * 1000).toLocaleString();
