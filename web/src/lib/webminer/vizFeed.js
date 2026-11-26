// The adapter between the web miner's workers and the page's visualisations: what the worker's
// {type:'viz', line} messages carry (pow/miner/src/rrt/child.rs `viz`, one JSON line about once a second
// per running search tree) and the campaign's log lines, parsed into plain objects. The page never
// looks at the raw lines elsewhere, so a change of the feed's format is a change here.
//
// A viz line: {step:{frame0,seed,goal,level,area,pos}, t, nodes_total, sim_frames,
//   best:{id,frame,stars,keys,pos}, nodes:[[id,parent,frame,x,y,z,stars,level,area,flags,kind?]], pads:[b,sx,sy,...],
//   kinds:[objective kind names] (only when the step's table grew: a node's `kind` indexes it; older workers send neither)
//   cov:{places,records,new:[floor keys]}}
// `clip` (at most one per ~2 s): a highlight, see child.rs `viz`; the node's path is in the mirror (treeMirror.js).
// Optional (not sent yet, used when present): `prefix`, the pads (same flat layout) the campaign
// had played before this step began, so the step's best path can be replayed from the block's start.

/** A worker's viz line as an object, or null. */
export function parseViz(line) {
  let m;
  try { m = JSON.parse(line); } catch { return null; }
  if (!m || !m.step) return null;
  const s = m.step;
  return {
    step: { frame0: s.frame0 ?? 0, seed: s.seed ?? 0, goal: String(s.goal ?? ''), level: s.level ?? 0, area: s.area ?? 1, pos: s.pos || [0, 0, 0] },
    key: `${s.seed}:${s.frame0}:${s.level}:${s.area}:${s.goal}`,
    t: m.t || 0,
    nodesTotal: m.nodes_total || 0,
    simFrames: m.sim_frames || 0,
    best: m.best || null,
    nodes: m.nodes || [],
    kinds: Array.isArray(m.kinds) ? m.kinds.map(String) : null, // the step's objective-kind table (nodes' 11th field indexes it)
    pads: m.pads || null,
    prefix: m.prefix || null,
    cov: m.cov || null, // {places, records, new: [floor keys]}: the tree's coverage of the step (pages/automine/coverage.js)
    clip: m.clip || null, // the step's highlight: {kind: 'place'|'record', id, frame, gain, pos, level, area}
  };
}

/**
 * A campaign log line (rrt::mine's `log`) as an event, or null:
 *  {kind:'step', n, name, frame, stars, keys, budget (M frames)}  "step 3: Bob at frame 812, 0 stars 0 keys: ...; budget 400 M frames"
 *  {kind:'done', name, frame, plus, mframes, nodes, secs}          "  Bob at frame 1234 (+422), 12.3 M frames, 51234 nodes (31 s)"
 *  {kind:'note', text}                                              anything else
 */
export function parseLog(line) {
  let m = /^step (\d+): (.+?) at frame (\d+), (\d+) stars (\d+) keys.*?budget ([\d.]+) M frames/.exec(line);
  if (m) return { kind: 'step', n: +m[1], name: m[2], frame: +m[3], stars: +m[4], keys: +m[5], budget: +m[6] };
  m = /^step (\d+): (.+?) at frame (\d+).*?budget ([\d.]+) M frames/.exec(line);
  if (m) return { kind: 'step', n: +m[1], name: m[2], frame: +m[3], stars: -1, keys: -1, budget: +m[4] };
  m = /^\s+(.+?) at frame (\d+) \(\+(\d+)\), ([\d.]+) M frames, (\d+) nodes \((\d+) s\)/.exec(line);
  if (m) return { kind: 'done', name: m[1], frame: +m[2], plus: +m[3], mframes: +m[4], nodes: +m[5], secs: +m[6] };
  m = /^\s+progress at frame (\d+) \(\+(\d+)\), ([\d.]+) M frames, (\d+) nodes \((\d+) s\)/.exec(line);
  if (m) return { kind: 'done', name: 'progress', frame: +m[1], plus: +m[2], mframes: +m[3], nodes: +m[4], secs: +m[5] };
  return { kind: 'note', text: line };
}

/** Flat pads [button, sx, sy, ...] -> raw bytes (4 per frame: u16 LE buttons, i8 x, i8 y). */
export function flatToRaw(flat) {
  const n = Math.floor(flat.length / 3);
  const out = new Uint8Array(n * 4);
  for (let i = 0; i < n; i++) {
    out[i * 4] = flat[i * 3] & 0xff;
    out[i * 4 + 1] = (flat[i * 3] >> 8) & 0xff;
    out[i * 4 + 2] = flat[i * 3 + 1] & 0xff;
    out[i * 4 + 3] = flat[i * 3 + 2] & 0xff;
  }
  return out;
}

/** The unpoked inputs of a submitted proof (proofFromRun's layout: varint frames, start byte, input_hash (32), codec 0, raw pads). */
export function proofInputs(hex) {
  const b = new Uint8Array(hex.length / 2);
  for (let i = 0; i < b.length; i++) b[i] = parseInt(hex.substr(i * 2, 2), 16);
  let i = 0, frames = 0, mul = 1;
  for (;;) { const x = b[i++]; frames += (x & 0x7f) * mul; if (x < 0x80) break; mul *= 128; }
  const power = b[i++] === 1;
  i += 32; // the committed input_hash
  const codec = b[i++];
  if (codec !== 0) throw new Error(`proof codec ${codec}`);
  const pads = [];
  for (; i + 4 <= b.length; i += 4) {
    pads.push({ button: b[i] | (b[i + 1] << 8), stick_x: (b[i + 2] << 24) >> 24, stick_y: (b[i + 3] << 24) >> 24 });
  }
  return { frames, power, pads };
}
