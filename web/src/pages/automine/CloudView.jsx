import React, { useEffect, useRef, useState } from 'react';
import { CAP } from './hub.js';
import { useHub, useVisible } from './useHub.js';
import { e2eHook } from '../../lib/e2e.js';
import { levelName, levelIcon, workerColor } from './levels.js';
import { Panel, Toolbar, ToolBtn } from '../../components/ui.jsx';
import { isTouchDevice } from '../../components/TouchPad.jsx';

// The explored search nodes as a 3D point cloud, with sm64-viewer's tree view controls (frontend/src/tree/treeview.js):
// the sampled nodes of every worker's trees in one level/area at a time, over that area's collision geometry (read from
// the player's own ROM). Left drag pans in the screen plane, middle drag pans across the ground, right drag orbits,
// the wheel zooms, a left click without dragging selects a node. Geometry is solid, see-through or off (areas with a roof
// start see-through; a choice is remembered per area), the cloud can be mirrored, edges (to each node's parent) toggled,
// and the view fitted.
// Hybrid mining adds: "Play from here" on a selected node (a person plays on from its state, see HybridPlay.jsx; the
// played segment becomes a manual node, drawn magenta, and the view highlights where it finished), and shift + left
// click (without dragging) on the level geometry, which sets a position goal for the searches (Mario::UserGoal to the point).
// three.js is loaded when this view mounts, i.e. only with the visualisations on.

const VIRIDIS = ['#440154', '#482878', '#3e4989', '#31688e', '#26828e', '#1f9e89', '#35b779', '#6ece58', '#b5de2b', '#fde725'];
const STAR_COLORS = ['#7c8aa5', '#60a5fa', '#4ade80', '#f7c948', '#fb923c', '#f472b6', '#a78bfa', '#2dd4bf'];
const MODES = [['stars', 'Stars'], ['frame', 'Game time'], ['worker', 'Worker'], ['progress', 'Made progress']];
const clock = (f) => { const s = f / 30; return `${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, '0')}`; };

// Geometry is solid by default; areas with a roof or ceiling over the play space start see-through so the tree inside
// shows ("level:area", or a level for all its areas). From sm64-viewer's treeview.js.
const SEE_THROUGH = new Set([
  '6', '4', '7', '14', '28', '18', '20', '27', '8:2', '8:3', '12:2', '22:2', '5:2', '10:2', '13:3', '23:1', '36:2', '36:3', '36:4',
]);
const GEOM_PREFS = 'obrrt-tree:geometry';
function geomPrefs() {
  try { return JSON.parse(localStorage.getItem(GEOM_PREFS) ?? '{}'); } catch { return {}; }
}
export function defaultGeomMode(key) {
  const saved = geomPrefs()[key];
  if (saved) return saved;
  return SEE_THROUGH.has(key) || SEE_THROUGH.has(key.split(':')[0]) ? 'ghost' : 'solid';
}
// Surface colours (treeview.js): floors light, walls darker, ceilings darkest; special surfaces by what they do.
function surfaceColor(type, ny) {
  if (/PAINTING/.test(type)) return '#c9a227';
  if (/BURNING/.test(type)) return '#b8442e';
  if (/WATER/.test(type)) return '#2f6fb0';
  if (/WARP/.test(type)) return '#8a5cc7';
  if (/HANGABLE/.test(type)) return '#4f8a6a';
  if (/SLIPPERY/.test(type) && !/NOT_SLIPPERY/.test(type)) return ny > 0.5 ? '#7d8fae' : '#4a5670';
  return ny > 0.5 ? '#6b7486' : ny < -0.5 ? '#2a2f3b' : '#454c5c';
}

const CLOUD_KEY = 'sm64coin.cloudSettings';
const loadCloud = () => { try { return { open: false, mode: 'stars', edges: true, ...(JSON.parse(localStorage.getItem(CLOUD_KEY) || 'null') || {}) }; } catch { return { open: false, mode: 'stars', edges: true }; } };

export default function CloudView({ hub, onPlay, onPlayFrom, onGoal, goalShare = 0.35, onGoalShare, canPlayFrom = false, whyNot = '' }) {
  const v = useHub(hub);
  const box = useRef(null);
  const mount = useRef(null);
  const visible = useVisible(box);
  const vis = useRef(visible);
  vis.current = visible;
  const gl = useRef(null);
  const [saved, setSaved] = useState(loadCloud);
  const { open, mode, edges } = saved;
  const patch = (p) => setSaved((o) => { const n = { ...o, ...p }; try { localStorage.setItem(CLOUD_KEY, JSON.stringify(n)); } catch { /* private window */ } return n; });
  const setMode = (m) => patch({ mode: m });
  const setEdges = (e) => patch({ edges: e });
  const [zone, setZone] = useState('auto');
  const [geom, setGeom] = useState('solid');
  const [mirror, setMirror] = useState(false);
  const [sel, setSel] = useState(null);
  const [err, setErr] = useState('');
  const cfg = useRef({ mode, zone, geom, edges, mirror });
  cfg.current = { mode, zone, geom, edges, mirror };
  const cb = useRef({});
  const [goalArm, setGoalArm] = useState(false);
  const [goalNote, setGoalNote] = useState('');
  const [share, setShare] = useState(goalShare);
  cb.current = { onGoal, armed: goalArm, disarm: () => setGoalArm(false), note: setGoalNote };

  // The zones with points, newest first.
  const zones = new Map();
  const n = hub.count();
  for (let i = 0; i < n; i += Math.max(1, Math.floor(n / 4000))) {
    const k = `${hub.level[i]}:${hub.area[i]}`;
    zones.set(k, (zones.get(k) || 0) + 1);
  }
  const latest = n ? (hub.n - 1) % CAP : -1;
  const autoZone = latest >= 0 ? `${hub.level[latest]}:${hub.area[latest]}` : null;
  const active = zone === 'auto' ? autoZone : zone;

  // A new area: its remembered (or default) geometry mode.
  useEffect(() => { if (active) setGeom(defaultGeomMode(active)); }, [active]);

  // A human-played segment just finished: show its area.
  const man = hub.manual;
  useEffect(() => {
    if (man) { setZone(`${man.level}:${man.area}`); gl.current?.focusManual(); }
  }, [man?.at]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    let dead = false, raf = 0, cleanup = () => {};
    (async () => {
      try {
        const THREE = await import('three');
        const { OrbitControls } = await import('three/examples/jsm/controls/OrbitControls.js');
        if (dead) return;
        const el = mount.current;
        const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: false });
        renderer.setPixelRatio(Math.min(2, window.devicePixelRatio || 1));
        el.appendChild(renderer.domElement);
        const dom = renderer.domElement;
        const scene = new THREE.Scene();
        scene.background = new THREE.Color(0x0e1015);
        const camera = new THREE.PerspectiveCamera(50, 1, 1, 1e6);
        camera.position.set(0, 4000, 6000);
        const controls = new OrbitControls(camera, dom);
        controls.enableDamping = true;
        // sm64-viewer's mouse: left drag pans in the screen plane, middle drag pans across the ground (the horizontal
        // plane), right drag orbits, the wheel zooms; a left click without dragging selects a node.
        controls.mouseButtons = { LEFT: THREE.MOUSE.PAN, MIDDLE: THREE.MOUSE.PAN, RIGHT: THREE.MOUSE.ROTATE };
        dom.addEventListener('pointerdown', (e) => { controls.screenSpacePanning = e.button === 0; }, true);
        // Stop the browser's middle-click autoscroll and the context menu over the view.
        dom.addEventListener('mousedown', (e) => { if (e.button === 1) e.preventDefault(); });
        dom.addEventListener('contextmenu', (e) => e.preventDefault());
        // Everything is drawn in game units inside `world`, so mirroring is one scale.
        const world = new THREE.Group();
        scene.add(world);
        scene.add(new THREE.AmbientLight(0xffffff, 0.55));
        const sun = new THREE.DirectionalLight(0xffffff, 0.9);
        sun.position.set(0.4, 1, 0.3);
        scene.add(sun);
        let grid = null;
        const geo = new THREE.BufferGeometry();
        geo.setAttribute('position', new THREE.BufferAttribute(new Float32Array(CAP * 3), 3));
        geo.setAttribute('color', new THREE.BufferAttribute(new Float32Array(CAP * 3), 3));
        geo.setDrawRange(0, 0);
        const mat = new THREE.PointsMaterial({ size: 5, sizeAttenuation: false, vertexColors: true, transparent: true, opacity: 0.9 });
        const points = new THREE.Points(geo, mat);
        points.frustumCulled = false;
        world.add(points);
        // The human-played nodes (flag bit 1), bigger and magenta.
        const mgeo = new THREE.BufferGeometry();
        mgeo.setAttribute('position', new THREE.BufferAttribute(new Float32Array(3 * 2048), 3));
        mgeo.setDrawRange(0, 0);
        const mpoints = new THREE.Points(mgeo, new THREE.PointsMaterial({ size: 11, sizeAttenuation: false, color: 0xff3df2 }));
        mpoints.frustumCulled = false;
        world.add(mpoints);
        // Edges: each node to its parent (same area).
        const egeo = new THREE.BufferGeometry();
        egeo.setAttribute('position', new THREE.BufferAttribute(new Float32Array(CAP * 6), 3));
        egeo.setAttribute('color', new THREE.BufferAttribute(new Float32Array(CAP * 6), 3));
        egeo.setDrawRange(0, 0);
        const elines = new THREE.LineSegments(egeo, new THREE.LineBasicMaterial({ vertexColors: true, transparent: true, opacity: 0.5 }));
        elines.frustumCulled = false;
        world.add(elines);
        const ray = new THREE.Raycaster();
        const mouse = new THREE.Vector2();
        // Selection: a marker and the ancestor chain.
        const marker = new THREE.Points(new THREE.BufferGeometry().setFromPoints([new THREE.Vector3()]), new THREE.PointsMaterial({ size: 14, sizeAttenuation: false, color: 0xf7c948 }));
        marker.visible = false;
        marker.frustumCulled = false;
        world.add(marker);
        const chainGeo = new THREE.BufferGeometry();
        const chainLine = new THREE.Line(chainGeo, new THREE.LineBasicMaterial({ color: 0xffffff }));
        chainLine.visible = false;
        chainLine.frustumCulled = false;
        world.add(chainLine);
        // The position goal: a ring of the objective's radius (150) and a pole.
        const goalG = new THREE.Group();
        goalG.add(new THREE.Mesh(new THREE.SphereGeometry(150, 16, 10), new THREE.MeshBasicMaterial({ color: 0xff8a00, wireframe: true, transparent: true, opacity: 0.7 })));
        goalG.add(new THREE.Line(new THREE.BufferGeometry().setFromPoints([new THREE.Vector3(0, 0, 0), new THREE.Vector3(0, 700, 0)]), new THREE.LineBasicMaterial({ color: 0xff8a00 })));
        const flag = new THREE.Mesh(new THREE.ConeGeometry(60, 160, 4), new THREE.MeshBasicMaterial({ color: 0xff8a00 }));
        flag.position.set(0, 700, 0);
        flag.rotation.x = Math.PI;
        goalG.add(flag);
        goalG.visible = false;
        world.add(goalG);
        // Where the last human-played segment finished: a ring that pulses, and the trail it took.
        const finishG = new THREE.Group();
        const fringA = new THREE.Mesh(new THREE.SphereGeometry(90, 16, 10), new THREE.MeshBasicMaterial({ color: 0xff3df2, wireframe: true }));
        const fringB = new THREE.Mesh(new THREE.TorusGeometry(260, 14, 8, 40), new THREE.MeshBasicMaterial({ color: 0x3df2ff }));
        fringB.rotation.x = Math.PI / 2;
        finishG.add(fringA, fringB);
        finishG.visible = false;
        world.add(finishG);
        const trail = new THREE.Line(new THREE.BufferGeometry(), new THREE.LineBasicMaterial({ color: 0x3df2ff }));
        trail.visible = false;
        trail.frustumCulled = false;
        world.add(trail);
        let geomMesh = null, geomKey = null, slotOf = new Int32Array(CAP), shown = 0, builtVer = -1, builtCfg = '', fitted = null, drawnMan = 0, drawnGoal = null;
        const cols = geo.attributes.color.array;
        const poss = geo.attributes.position.array;
        const ecols = egeo.attributes.color.array;
        const eposs = egeo.attributes.position.array;
        const tmp = new THREE.Color();
        const scale = (stops, t) => { const x = Math.max(0, Math.min(0.9999, t)) * (stops.length - 1); const i = Math.floor(x); return new THREE.Color(stops[i]).lerp(new THREE.Color(stops[i + 1]), x - i); };
        const palette = {
          viridis: Array.from({ length: 64 }, (_, i) => scale(VIRIDIS, i / 63)),
          stars: STAR_COLORS.map((c) => new THREE.Color(c)),
        };

        async function loadGeometry(key) {
          if (geomKey === key) { applyGeom(); return; }
          if (geomMesh) { world.remove(geomMesh); geomMesh.traverse((o) => o.geometry?.dispose()); geomMesh = null; }
          geomKey = key;
          if (!key) return;
          const [level, area] = key.split(':').map(Number);
          try {
            const { romGeometry } = await import('./geometry.js');
            const d = await romGeometry(level, area);
            if (dead || geomKey !== key || !d.t.length) return;
            const v3 = d.v, t = d.t;
            const pos = [], col = [], c = new THREE.Color();
            const a = new THREE.Vector3(), b = new THREE.Vector3(), e = new THREE.Vector3(), nrm = new THREE.Vector3();
            for (let i = 0; i < t.length / 3; i++) {
              const type = d.types[d.s[i]] ?? '';
              if (/DEATH_PLANE/.test(type)) continue;
              const [i0, i1, i2] = [t[3 * i], t[3 * i + 1], t[3 * i + 2]];
              a.set(v3[3 * i0], v3[3 * i0 + 1], v3[3 * i0 + 2]); b.set(v3[3 * i1], v3[3 * i1 + 1], v3[3 * i1 + 2]); e.set(v3[3 * i2], v3[3 * i2 + 1], v3[3 * i2 + 2]);
              nrm.subVectors(b, a).cross(new THREE.Vector3().subVectors(e, a)).normalize();
              c.set(surfaceColor(type, nrm.y));
              for (const p of [a, b, e]) { pos.push(p.x, p.y, p.z); col.push(c.r, c.g, c.b); }
            }
            const g = new THREE.BufferGeometry();
            g.setAttribute('position', new THREE.Float32BufferAttribute(pos, 3));
            g.setAttribute('color', new THREE.Float32BufferAttribute(col, 3));
            g.computeVertexNormals();
            const group = new THREE.Group();
            group.add(new THREE.Mesh(g, new THREE.MeshLambertMaterial({ vertexColors: true, side: THREE.DoubleSide, polygonOffset: true, polygonOffsetFactor: 2, polygonOffsetUnits: 2 })));
            group.renderOrder = -1;
            geomMesh = group;
            world.add(group);
            applyGeom();
          } catch (e) { if (!dead) setErr(`level geometry: ${e.message || e}`); }
        }

        function applyGeom() {
          if (!geomMesh) return;
          const m = geomMesh.children[0].material;
          const gm = cfg.current.geom;
          geomMesh.visible = gm !== 'off';
          m.transparent = gm === 'ghost';
          m.opacity = gm === 'ghost' ? 0.28 : 1;
          m.depthWrite = gm !== 'ghost';
          m.needsUpdate = true;
        }

        function applyMirror() {
          world.scale.z = cfg.current.mirror ? -1 : 1;
          world.updateMatrixWorld(true);
        }

        const zoneOf = (i) => `${hub.level[i]}:${hub.area[i]}`;
        function rebuild() {
          const { mode: m, zone: z, edges: showEdges } = cfg.current;
          const zk = z === 'auto' ? (hub.n ? zoneOf((hub.n - 1) % CAP) : null) : z;
          const total = hub.count();
          let k = 0, fmin = Infinity, fmax = 0, mk = 0, ek = 0;
          for (let i = 0; i < total; i++) { if (zoneOf(i) === zk) { if (hub.frame[i] < fmin) fmin = hub.frame[i]; if (hub.frame[i] > fmax) fmax = hub.frame[i]; } }
          for (let i = 0; i < total; i++) {
            if (zoneOf(i) !== zk) continue;
            poss[k * 3] = hub.pos[i * 3]; poss[k * 3 + 1] = hub.pos[i * 3 + 1]; poss[k * 3 + 2] = hub.pos[i * 3 + 2];
            let c;
            if (m === 'stars') c = palette.stars[hub.stars[i] % palette.stars.length];
            else if (m === 'frame') c = palette.viridis[Math.round(((hub.frame[i] - fmin) / Math.max(1, fmax - fmin)) * 63)];
            else if (m === 'worker') { tmp.set(workerColor(hub.worker[i])); c = tmp; }
            else c = hub.flags[i] & 1 ? palette.stars[3] : palette.stars[0];
            cols[k * 3] = c.r; cols[k * 3 + 1] = c.g; cols[k * 3 + 2] = c.b;
            slotOf[k] = i;
            if (hub.flags[i] & 2 && mk < 2048) { mgeo.attributes.position.array.set([hub.pos[i * 3], hub.pos[i * 3 + 1], hub.pos[i * 3 + 2]], mk * 3); mk++; }
            const p = hub.parentSlot[i];
            if (showEdges && p >= 0 && zoneOf(p) === zk) {
              eposs[ek * 6] = hub.pos[p * 3]; eposs[ek * 6 + 1] = hub.pos[p * 3 + 1]; eposs[ek * 6 + 2] = hub.pos[p * 3 + 2];
              eposs[ek * 6 + 3] = hub.pos[i * 3]; eposs[ek * 6 + 4] = hub.pos[i * 3 + 1]; eposs[ek * 6 + 5] = hub.pos[i * 3 + 2];
              tmp.set(workerColor(hub.worker[i])).multiplyScalar(0.45);
              ecols[ek * 6] = ecols[ek * 6 + 3] = tmp.r; ecols[ek * 6 + 1] = ecols[ek * 6 + 4] = tmp.g; ecols[ek * 6 + 2] = ecols[ek * 6 + 5] = tmp.b;
              ek++;
            }
            k++;
          }
          shown = k;
          geo.setDrawRange(0, k);
          geo.attributes.position.needsUpdate = true;
          geo.attributes.color.needsUpdate = true;
          geo.boundingSphere = null;
          mgeo.setDrawRange(0, mk);
          mgeo.attributes.position.needsUpdate = true;
          mgeo.boundingSphere = null;
          egeo.setDrawRange(0, ek * 2);
          egeo.attributes.position.needsUpdate = true;
          egeo.attributes.color.needsUpdate = true;
          egeo.boundingSphere = null;
          if (zk !== fitted && k > 0) fitView(zk);
          loadGeometry(zk);
          drawGoal();
          drawManual();
          doFocus();
          const s = hub.selected;
          select(s && s.slot != null && zoneOf(s.slot) === zk ? s.slot : null);
        }

        function fitView(zk = fitted) {
          const kk = shown;
          if (!kk) return;
          fitted = zk;
          const box3 = new THREE.Box3();
          const mz = cfg.current.mirror ? -1 : 1;
          for (let q = 0; q < kk; q++) box3.expandByPoint(new THREE.Vector3(poss[q * 3], poss[q * 3 + 1], mz * poss[q * 3 + 2]));
          const c = box3.getCenter(new THREE.Vector3()), size = box3.getSize(new THREE.Vector3());
          const r = Math.max(size.x, size.y, size.z, 500);
          controls.target.copy(c);
          // Behind and above the area's centre, looking towards -z (into the castle from the grounds).
          camera.position.set(c.x, c.y + r * 0.8, c.z + mz * r * 0.9);
          camera.near = r / 1000; camera.far = r * 50; camera.updateProjectionMatrix();
          if (grid) { scene.remove(grid); grid.geometry.dispose(); }
          grid = new THREE.GridHelper(Math.max(size.x, size.z) * 1.3 + 400, 24, 0x2b3242, 0x1b1f2a);
          grid.position.set(c.x, box3.min.y - 5, c.z);
          scene.add(grid);
          ray.params.Points.threshold = r / 150;
        }

        // The camera to the last human-played segment's finish (once its area is the one shown), keeping its distance.
        let pendingFocus = false;
        function doFocus() {
          const m = hub.manual;
          if (!pendingFocus || !m || fitted !== `${m.level}:${m.area}`) return;
          pendingFocus = false;
          const t = new THREE.Vector3(m.pos[0], m.pos[1], (cfg.current.mirror ? -1 : 1) * m.pos[2]);
          camera.position.add(t.clone().sub(controls.target));
          controls.target.copy(t);
        }

        function select(slot) {
          if (slot == null) { marker.visible = false; chainLine.visible = false; return; }
          marker.position.set(hub.pos[slot * 3], hub.pos[slot * 3 + 1], hub.pos[slot * 3 + 2]);
          marker.visible = true;
          // The path from the area's entry to it (the ancestors that were sampled).
          const ch = [slot, ...hub.chain(slot)].filter((s) => zoneOf(s) === zoneOf(slot));
          const arr = new Float32Array(ch.length * 3);
          ch.forEach((s, i) => arr.set([hub.pos[s * 3], hub.pos[s * 3 + 1], hub.pos[s * 3 + 2]], i * 3));
          chainGeo.setAttribute('position', new THREE.BufferAttribute(arr, 3));
          chainGeo.computeBoundingSphere();
          chainLine.visible = ch.length > 1;
        }

        function drawGoal() {
          const g = hub.goal;
          const zk = fitted;
          drawnGoal = g;
          goalG.visible = !!g && zk === `${g.level}:${g.area}`;
          if (g) goalG.position.set(g.pos[0], g.pos[1], g.pos[2]);
        }

        function drawManual() {
          const m = hub.manual;
          drawnMan = m ? m.at : 0;
          const here = m && fitted === `${m.level}:${m.area}`;
          finishG.visible = !!here;
          trail.visible = false;
          if (!m) return;
          if (here) finishG.position.set(m.pos[0], m.pos[1], m.pos[2]);
          if (m.trail && m.trail.length >= 6 && here) {
            trail.geometry.dispose();
            trail.geometry = new THREE.BufferGeometry();
            trail.geometry.setAttribute('position', new THREE.Float32BufferAttribute(m.trail, 3));
            trail.visible = true;
          }
        }

        const snapshot = (slot) => ({
          slot, w: hub.worker[slot], id: hub.nodeId[slot], frame: hub.frame[slot], stars: hub.stars[slot], level: hub.level[slot], area: hub.area[slot],
          pos: [hub.pos[slot * 3], hub.pos[slot * 3 + 1], hub.pos[slot * 3 + 2]], progress: hub.flags[slot] & 1, manual: (hub.flags[slot] >> 1) & 1,
          chain: hub.chain(slot).length, step: hub.stepOf[slot], current: hub.isCurrent(slot),
        });
        const setRay = (ev) => {
          const r = dom.getBoundingClientRect();
          mouse.set(((ev.clientX - r.left) / r.width) * 2 - 1, -((ev.clientY - r.top) / r.height) * 2 + 1);
          ray.setFromCamera(mouse, camera);
        };
        // Picking is in SCREEN space: the shown points are projected to pixels and the one nearest to the pointer within a
        // radius (a finger is fatter than a mouse) wins; ties (within 2 px) go to the one nearest the camera, so the node
        // under the finger is picked, not one far behind it on another floor.
        const onSelect = (ev) => {
          const R = ev.pointerType === 'touch' ? 24 : 10;
          const r = dom.getBoundingClientRect();
          world.updateMatrixWorld(true); camera.updateMatrixWorld();
          const M = new THREE.Matrix4().multiplyMatrices(camera.projectionMatrix, camera.matrixWorldInverse).multiply(world.matrixWorld).elements;
          const px0 = ev.clientX, py0 = ev.clientY;
          let bestK = -1, bestD = Infinity, bestW = Infinity;
          const cand = [];
          for (let k = 0; k < shown; k++) {
            const x = poss[k * 3], y = poss[k * 3 + 1], z = poss[k * 3 + 2];
            const w = M[3] * x + M[7] * y + M[11] * z + M[15];
            if (w <= 0) continue;
            const sx = r.left + ((M[0] * x + M[4] * y + M[8] * z + M[12]) / w + 1) / 2 * r.width;
            const sy = r.top + (1 - (M[1] * x + M[5] * y + M[9] * z + M[13]) / w) / 2 * r.height;
            const d = Math.hypot(sx - px0, sy - py0);
            if (d > R) continue;
            cand.push([k, d, w]);
            if (d < bestD) { bestD = d; }
          }
          for (const [k, d, w] of cand) if (d <= bestD + 2 && w < bestW) { bestW = w; bestK = k; }
          if (bestK < 0) { hub.selected = null; setSel(null); select(null); return; }
          const slot = slotOf[bestK];
          hub.selected = { slot };
          setSel(snapshot(slot));
          select(slot);
        };
        // The position goal: the level geometry under the pointer. A ray crosses several upward-facing surfaces (a roof's top,
        // the floors below it); the floor where the search has been (nearest to a shown node, within 300 units) wins, else
        // the first one. The same spot again within 3 s (shift + click again, or another long press) takes the next floor down.
        let lastGoal = null; // {x, y, t, layers: [{p, d}], i}
        const onGoal = (ev) => {
          if (!geomMesh || !fitted) return;
          setRay(ev);
          world.updateMatrixWorld(true);
          const hits = ray.intersectObject(geomMesh, true);
          const now = performance.now();
          const again = lastGoal && now - lastGoal.t < 3000 && Math.hypot(ev.clientX - lastGoal.x, ev.clientY - lastGoal.y) < 14;
          let layers, i;
          if (again) {
            layers = lastGoal.layers; i = (lastGoal.i + 1) % layers.length;
          } else {
            const floors = [];
            for (const h of hits) {
              if (!h.face || h.face.normal.y <= 0.5) continue;
              const p = world.worldToLocal(h.point.clone());
              if (floors.some((f) => Math.abs(f.p.y - p.y) < 40)) continue; // one per layer
              let dmin = Infinity;
              for (let k = 0; k < shown; k++) { const d = Math.hypot(poss[k * 3] - p.x, poss[k * 3 + 1] - p.y, poss[k * 3 + 2] - p.z); if (d < dmin) dmin = d; }
              floors.push({ p, d: dmin });
            }
            if (!floors.length && hits[0]) floors.push({ p: world.worldToLocal(hits[0].point.clone()), d: Infinity });
            if (!floors.length) return;
            layers = floors.sort((a, b) => b.p.y - a.p.y); // top to bottom
            let near = -1;
            layers.forEach((f, k) => { if (f.d <= 300 && (near < 0 || f.d < layers[near].d)) near = k; });
            i = near >= 0 ? near : 0;
          }
          lastGoal = { x: ev.clientX, y: ev.clientY, t: now, layers, i };
          const p = layers[i].p;
          const [level, area] = fitted.split(':').map(Number);
          cb.current.onGoal?.({ level, area, pos: [Math.round(p.x), Math.round(p.y), Math.round(p.z)] });
          cb.current.note?.(`Floor ${i + 1}/${layers.length}`);
        };
        let downAt = null;
        // Touch: no shift key, so a long press (~500 ms, one finger, moved < 8 px) on the level sets the goal, and the
        // "Goal" toggle makes the next tap do it. A ring over the press fills while it counts; moving or a second finger
        // cancels it; when it fires the little pan the finger made is undone and the controls stay off until it lifts.
        const LONG_MS = 500, SLOP = 8;
        const ring = document.createElement('div');
        ring.className = 'viz-ring';
        ring.style.cssText = 'position:absolute;display:none;width:56px;height:56px;margin:-28px 0 0 -28px;border-radius:50%;pointer-events:none;z-index:3;-webkit-mask:radial-gradient(circle,transparent 19px,#000 20px);mask:radial-gradient(circle,transparent 19px,#000 20px)';
        if (getComputedStyle(el).position === 'static') el.style.position = 'relative';
        el.appendChild(ring);
        const touches = new Set();
        let lp = null; // {id, x, y, t0, cam, tgt, fired}
        let lpRaf = 0;
        let suppress = false;
        const lpEnd = () => { if (lpRaf) cancelAnimationFrame(lpRaf); lpRaf = 0; ring.style.display = 'none'; lp = null; };
        const lpTick = () => {
          if (!lp) return;
          const f = Math.min(1, (performance.now() - lp.t0) / LONG_MS);
          ring.style.background = `conic-gradient(#ff8a00 ${f * 360}deg, rgba(255,138,0,.25) 0)`;
          if (f >= 1) {
            const l = lp;
            camera.position.copy(l.cam); controls.target.copy(l.tgt); controls.update();
            controls.enabled = false;
            l.fired = true;
            try { navigator.vibrate?.(40); } catch { /* not supported */ }
            onGoal({ clientX: l.x, clientY: l.y });
            ring.style.display = 'none'; lpRaf = 0;
            return;
          }
          lpRaf = requestAnimationFrame(lpTick);
        };
        dom.addEventListener('pointerdown', (e) => {
          downAt = [e.clientX, e.clientY, e.button];
          if (e.pointerType !== 'touch') return;
          touches.add(e.pointerId);
          if (touches.size > 1) { lpEnd(); return; }
          const r = el.getBoundingClientRect();
          lp = { id: e.pointerId, x: e.clientX, y: e.clientY, t0: performance.now(), cam: camera.position.clone(), tgt: controls.target.clone(), fired: false };
          ring.style.left = `${e.clientX - r.left}px`; ring.style.top = `${e.clientY - r.top}px`;
          ring.style.display = 'block';
          lpRaf = requestAnimationFrame(lpTick);
        });
        dom.addEventListener('pointermove', (e) => {
          if (lp && !lp.fired && e.pointerId === lp.id && Math.hypot(e.clientX - lp.x, e.clientY - lp.y) > SLOP) lpEnd();
        });
        const lpUp = (e) => {
          if (e.pointerType !== 'touch') return;
          touches.delete(e.pointerId);
          if (!touches.size) controls.enabled = true;
          if (lp && e.pointerId === lp.id) { const fired = lp.fired; lpEnd(); if (fired) { downAt = null; suppress = true; } }
        };
        dom.addEventListener('pointercancel', lpUp);
        dom.addEventListener('pointerup', lpUp);
        dom.addEventListener('pointerup', (e) => {
          if (suppress) { suppress = false; downAt = null; return; }
          if (downAt && downAt[2] === e.button && Math.hypot(e.clientX - downAt[0], e.clientY - downAt[1]) < 4) {
            if (e.button === 0 && (e.shiftKey || cb.current.armed)) { onGoal(e); if (cb.current.armed) cb.current.disarm?.(); }
            else if (e.button === 0) onSelect(e);
          }
          downAt = null;
        });

        const resize = () => {
          const w = el.clientWidth, h = Math.max(260, Math.round(w * 0.62));
          renderer.setSize(w, h);
          camera.aspect = w / h;
          camera.updateProjectionMatrix();
          renderer.render(scene, camera); // setSize clears the canvas: redraw at once (the frame loop skips off-screen views)
        };
        resize();
        const ro = new ResizeObserver(resize);
        ro.observe(el);
        gl.current = {
          fit: () => fitView(fitted),
          reset: () => { fitted = null; rebuild(); },
          applyGeom,
          applyMirror: () => { applyMirror(); fitView(fitted); },
          focusManual: () => { pendingFocus = true; doFocus(); },
          select: (slot) => { hub.selected = { slot }; setSel(snapshot(slot)); select(slot); },
        };
        let lastBuild = 0, lastHl = null;
        const frame = () => {
          raf = requestAnimationFrame(frame);
          if (!vis.current) return;
          const cf = JSON.stringify({ ...cfg.current, geom: undefined, mirror: undefined });
          const now = performance.now();
          if ((hub.cloudChanged || cf !== builtCfg) && now - lastBuild > 400) {
            hub.cloudChanged = false; builtCfg = cf; lastBuild = now; rebuild();
          } else if ((hub.manual ? hub.manual.at : 0) !== drawnMan) drawManual();
          else if (hub.goal !== drawnGoal) drawGoal();
          // The node of the highlight clip being played is marked like a clicked one.
          if (hub.hl !== lastHl) {
            lastHl = hub.hl;
            const hs = lastHl ? hub.slotOf(lastHl.w, lastHl.step, lastHl.id) : -1;
            if (hs >= 0 && zoneOf(hs) === fitted) select(hs);
          }
          // The finish marker pulses for a while.
          if (finishG.visible) {
            const age = (Date.now() - (hub.manual?.at || 0)) / 1000;
            const s = age < 12 ? 1 + 0.25 * Math.sin(age * 6) : 1;
            finishG.scale.setScalar(s);
          }
          controls.update();
          renderer.render(scene, camera);
        };
        frame();
        cleanup = () => {
          cancelAnimationFrame(raf);
          ro.disconnect();
          controls.dispose();
          geo.dispose(); mat.dispose(); mgeo.dispose(); egeo.dispose();
          if (geomMesh) geomMesh.traverse((o) => o.geometry?.dispose());
          renderer.dispose();
          renderer.forceContextLoss?.();
          renderer.domElement.remove();
          gl.current = null;
        };
        // The window's debug hook (the headless test and the console): the scene's pieces.
        e2eHook('__cloud', { camera, controls, world, scene, get shown() { return shown; }, get fitted() { return fitted; }, THREE, project: (x, y, z) => { const v = new THREE.Vector3(x, y, z).applyMatrix4(world.matrixWorld).project(camera); const r = dom.getBoundingClientRect(); return [r.left + (v.x + 1) / 2 * r.width, r.top + (1 - v.y) / 2 * r.height]; }, get geomMesh() { return geomMesh; }, slotOf: (k) => slotOf[k] });
        void builtVer;
      } catch (e) { setErr(String(e.message || e)); }
    })();
    return () => { dead = true; cleanup(); };
  }, []);
  useEffect(() => { gl.current?.applyGeom(); }, [geom]);
  useEffect(() => { gl.current?.applyMirror(); }, [mirror]);

  const w = sel ? hub.workers[sel.w] : null;
  const isBest = sel && w?.best && w.best.id === sel.id;
  const bestW = (() => {
    let bw = -1, bk = null;
    hub.workers.forEach((x, i) => { if (x.best) { const k = [x.best.stars + x.best.keys, -x.best.frameAbs]; if (!bk || k[0] > bk[0] || (k[0] === bk[0] && k[1] > bk[1])) { bk = k; bw = i; } } });
    return bw;
  })();
  const cycleGeom = () => {
    const next = { solid: 'ghost', ghost: 'off', off: 'solid' }[geom];
    setGeom(next);
    if (active) { try { localStorage.setItem(GEOM_PREFS, JSON.stringify({ ...geomPrefs(), [active]: next })); } catch { /* not remembered */ } }
  };
  const goal = hub.goal;
  // How near the tree has got: the closest sampled node in the goal's level and area.
  let goalNear = null;
  if (goal) {
    const n = Math.min(hub.n, CAP);
    for (let i = 0; i < n; i++) {
      if (hub.level[i] !== goal.level || hub.area[i] !== goal.area) continue;
      const dx = hub.pos[i * 3] - goal.pos[0], dy = hub.pos[i * 3 + 1] - goal.pos[1], dz = hub.pos[i * 3 + 2] - goal.pos[2];
      const d = Math.sqrt(dx * dx + dy * dy + dz * dz);
      if (goalNear === null || d < goalNear) goalNear = d;
    }
  }
  void v;
  const playHere = canPlayFrom && sel && sel.current;
  const why = !canPlayFrom ? whyNot : sel && !sel.current ? 'That node belongs to a search step that is over: pick a node of the current step.' : '';
  return (
    <Panel className="viz-cloud" innerRef={box} open={open} onToggle={() => patch({ open: !open })}
      title="Search cloud" chip={<span className="muted small">{hub.n.toLocaleString()} nodes{hub.n > CAP ? ` (newest ${CAP.toLocaleString()} kept)` : ''}</span>}
      settings={(
        <form className="viz-settings" onSubmit={(e) => e.preventDefault()}>
          <span className="lab">Colour by</span>
          <div className="viz-btns">{MODES.map(([k, l]) => <button type="button" key={k} className={mode === k ? 'on' : ''} onClick={() => setMode(k)}>{l}</button>)}</div>
          <label htmlFor="cloud-zone">Level and area</label>
          <div className="ctl">
            <select id="cloud-zone" value={zone} onChange={(e) => { setZone(e.target.value); gl.current?.reset(); }}>
              <option value="auto">follow the search{autoZone ? ` (${levelName(+autoZone.split(':')[0])})` : ''}</option>
              {[...zones.entries()].map(([k, c]) => { const [l, a] = k.split(':'); return <option key={k} value={k}>{levelIcon(+l)} {levelName(+l)} · area {a} ({c})</option>; })}
            </select>
          </div>
          <label htmlFor="cloud-goal">Goal strength</label>
          <div className="ctl">
            <input id="cloud-goal" type="range" min="0" max="100" step="5" value={Math.round(share * 100)} onChange={(e) => { const x = Number(e.target.value) / 100; setShare(x); onGoalShare && onGoalShare(x); }} title="The share of the searches' plans that start from the tree's state nearest the position goal and go for it (the rest search as usual)" /> <span className="muted">{Math.round(share * 100)}% of plans</span>
          </div>
          <span className="lab">View</span>
          <div className="viz-btns">
            <button type="button" className={edges ? 'on' : ''} onClick={() => setEdges(!edges)} title="Each node's edge to its parent">Edges</button>
            <button type="button" onClick={cycleGeom} title="Level collision geometry (from your ROM): solid, see-through or off; remembered per area">Geometry: {geom === 'ghost' ? 'see-through' : geom}</button>
            <button type="button" className={mirror ? 'on' : ''} onClick={() => setMirror(!mirror)} title="Mirror the view (z flipped)">Mirror</button>
            <button type="button" onClick={() => gl.current?.fit()}>Fit</button>
          </div>
          <span className="lab">Controls</span>
          <span className="muted">left drag pans, middle drag pans on the ground, right drag orbits, wheel zooms; left click selects a node; <b>shift + left click</b> on the level sets a position goal for the searches (a Mario::UserGoal objective at that point). On a touch screen: <b>long-press the level</b> (or use the 🎯 Goal button, then tap it); drag pans, pinch zooms, tap selects.</span>
        </form>
      )}>
      <div ref={mount} className="viz-gl" />
      {err && <p className="error small">{err}</p>}
      <div className="viz-status">
        {sel ? (
          <span><b>{sel.manual ? 'Manual node' : 'Node'} {sel.id}</b> of worker {sel.w + 1} · {levelIcon(sel.level)} {levelName(sel.level)} (area {sel.area}) · {sel.stars} ★ · frame {sel.frame.toLocaleString()} ({clock(sel.frame)}){sel.progress ? ' · made progress' : ''}</span>
        ) : <span className="muted">{goalArm ? 'Tap the level where the goal should be.' : isTouchDevice() ? 'Tap a node to select it; long-press the level to set a goal.' : 'Click a node to select it; shift + click on the level sets a position goal.'}</span>}
        {goal && <span className="muted small">{goalNote ? <b title="Which floor layer under the pointer the goal landed on (top = 1); the same spot again within 3 s picks the next one down">{goalNote} · </b> : null}Position goal active ({goal.pos.join(', ')}) in {levelName(goal.level)}, area {goal.area}: {Math.round(share * 100)}% of the searches' plans go for it · {goalNear === null ? 'no tree node in that area yet' : goalNear <= 150 ? 'reached' : `nearest node ${Math.round(goalNear).toLocaleString()} units away`}.</span>}
        {hub.manual && <span className="muted small">Last played segment: {hub.manual.nid != null ? `node ${hub.manual.nid} of worker ${hub.manual.w + 1}` : 'not in the tree'}, {hub.manual.frames} frames, finished at ({hub.manual.pos.map((x) => Math.round(x)).join(', ')}){hub.manual.note ? ` · ${hub.manual.note}` : ''}</span>}
      </div>
      <Toolbar label="Node actions">
        <ToolBtn on={!!playHere} disabled={!playHere} onClick={() => onPlayFrom && onPlayFrom(sel)} title={playHere ? 'Load this node\'s save state and play on by hand; the played segment becomes a high-weight node of the search tree' : (sel ? why || 'Only a node of a worker\'s current search step can be played from' : 'Select a node first')}>🎮 Play from here</ToolBtn>
        <ToolBtn disabled={!sel} onClick={() => onPlay && onPlay(sel.w, { step: sel.step, id: sel.id })} title="Replay the path to this node in the game panel">▶ {isBest ? 'This path' : 'This node'}</ToolBtn>
        <ToolBtn disabled={bestW < 0} onClick={() => bestW >= 0 && onPlay && onPlay(bestW)} title="Replay the best route so far in the game panel">▶ Best path</ToolBtn>
        <ToolBtn on={goalArm} onClick={() => setGoalArm(!goalArm)} title="Then tap the level to set the position goal there (long-press does it too; shift + click on a computer)">🎯 Goal</ToolBtn>
        {goal && <ToolBtn onClick={() => onGoal && onGoal(null)} title="Clear the position goal">✕ Goal</ToolBtn>}
      </Toolbar>
      {sel && !playHere && why && <p className="muted small" style={{ margin: '6px 0 0' }}>{why}</p>}
    </Panel>
  );
}
