import React, { useEffect, useRef, useState } from 'react';
import { CAP } from './hub.js';
import { Panel } from '../../components/ui.jsx';
import { useHub, useVisible } from './useHub.js';
import { levelName, levelIcon, workerColor } from './levels.js';

// The explored search nodes as a 3D point cloud (after sm64-viewer's tree view): the sampled nodes of every
// worker's trees in one level/area at a time, over that area's collision geometry (read from the player's
// own ROM). Orbit with the mouse, click a node for its details; "play this path" replays the worker's best
// path in the game view (the only path the feed carries inputs for).
// three.js is loaded when this view mounts, i.e. only with the visualisations on.

const VIRIDIS = ['#440154', '#482878', '#3e4989', '#31688e', '#26828e', '#1f9e89', '#35b779', '#6ece58', '#b5de2b', '#fde725'];
const STAR_COLORS = ['#7c8aa5', '#60a5fa', '#4ade80', '#f7c948', '#fb923c', '#f472b6', '#a78bfa', '#2dd4bf'];
const MODES = [['stars', 'Stars'], ['frame', 'Game time'], ['worker', 'Worker'], ['progress', 'Made progress']];
const clock = (f) => { const s = f / 30; return `${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, '0')}`; };

const CLOUD_KEY = 'sm64coin.cloudSettings';
const loadCloud = () => { try { return { open: false, mode: 'stars', geom: 'solid', ...(JSON.parse(localStorage.getItem(CLOUD_KEY) || 'null') || {}) }; } catch { return { open: false, mode: 'stars', geom: 'solid' }; } };

export default function CloudView({ hub, onPlay }) {
  const v = useHub(hub);
  const box = useRef(null);
  const mount = useRef(null);
  const visible = useVisible(box);
  const vis = useRef(visible);
  vis.current = visible;
  const gl = useRef(null);
  const [saved, setSaved] = useState(loadCloud);
  const { open, mode, geom } = saved;
  const patch = (p) => setSaved((o) => { const n = { ...o, ...p }; try { localStorage.setItem(CLOUD_KEY, JSON.stringify(n)); } catch { /* private window */ } return n; });
  const setMode = (m) => patch({ mode: m });
  const setGeom = (g) => patch({ geom: g });
  const [zone, setZone] = useState('auto');
  const [sel, setSel] = useState(null);
  const [err, setErr] = useState('');
  const cfg = useRef({ mode, zone, geom });
  cfg.current = { mode, zone, geom };

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
        const scene = new THREE.Scene();
        scene.background = new THREE.Color(0x0b0e1a);
        const camera = new THREE.PerspectiveCamera(50, 1, 20, 100000);
        camera.position.set(0, 4000, 6000);
        const controls = new OrbitControls(camera, renderer.domElement);
        controls.enableDamping = true;
        scene.add(new THREE.AmbientLight(0xffffff, 0.7));
        const sun = new THREE.DirectionalLight(0xffffff, 0.8);
        sun.position.set(0.4, 1, 0.3);
        scene.add(sun);
        const geo = new THREE.BufferGeometry();
        geo.setAttribute('position', new THREE.BufferAttribute(new Float32Array(CAP * 3), 3));
        geo.setAttribute('color', new THREE.BufferAttribute(new Float32Array(CAP * 3), 3));
        geo.setDrawRange(0, 0);
        const mat = new THREE.PointsMaterial({ size: 7, sizeAttenuation: false, vertexColors: true, transparent: true, opacity: 0.9 });
        const points = new THREE.Points(geo, mat);
        points.frustumCulled = false;
        scene.add(points);
        const ray = new THREE.Raycaster();
        const mouse = new THREE.Vector2();
        // Selection: a ring marker and the ancestor chain.
        const marker = new THREE.Mesh(new THREE.SphereGeometry(60, 12, 8), new THREE.MeshBasicMaterial({ color: 0xffffff, wireframe: true }));
        marker.visible = false;
        scene.add(marker);
        const chainGeo = new THREE.BufferGeometry();
        const chainLine = new THREE.Line(chainGeo, new THREE.LineBasicMaterial({ color: 0xffffff }));
        chainLine.visible = false;
        chainLine.frustumCulled = false;
        scene.add(chainLine);
        let mesh = null, meshKey = null, slotOf = new Int32Array(CAP), shown = 0, builtVer = -1, builtCfg = '', fitted = null;
        const cols = geo.attributes.color.array;
        const poss = geo.attributes.position.array;
        const tmp = new THREE.Color();
        const scale = (stops, t) => { const x = Math.max(0, Math.min(0.9999, t)) * (stops.length - 1); const i = Math.floor(x); return new THREE.Color(stops[i]).lerp(new THREE.Color(stops[i + 1]), x - i); };
        const palette = {
          viridis: Array.from({ length: 64 }, (_, i) => scale(VIRIDIS, i / 63)),
          stars: STAR_COLORS.map((c) => new THREE.Color(c)),
        };

        async function loadGeometry(key) {
          if (meshKey === key) return;
          meshKey = key;
          if (mesh) { scene.remove(mesh); mesh.geometry.dispose(); mesh.material.dispose(); mesh = null; }
          if (!key) return;
          const [level, area] = key.split(':').map(Number);
          try {
            const { romGeometry } = await import('./geometry.js');
            const g = await romGeometry(level, area);
            if (dead || meshKey !== key || !g.t.length) return;
            const nTri = g.t.length / 3;
            const p = new Float32Array(nTri * 9), c = new Float32Array(nTri * 9);
            const bad = new Set(g.types.map((nm, i) => (/DEATH|BURNING|QUICKSAND|WATER|INTANGIBLE/.test(nm) ? i : -1)).filter((i) => i >= 0));
            for (let i = 0; i < nTri; i++) {
              const a = [], b = [];
              for (let k = 0; k < 3; k++) { const vi = g.t[i * 3 + k] * 3; a.push(g.v[vi], g.v[vi + 1], g.v[vi + 2]); p.set([g.v[vi], g.v[vi + 1], g.v[vi + 2]], i * 9 + k * 3); }
              const ux = a[3] - a[0], uy = a[4] - a[1], uz = a[5] - a[2], wx = a[6] - a[0], wy = a[7] - a[1], wz = a[8] - a[2];
              let nx = uy * wz - uz * wy, ny = uz * wx - ux * wz, nz = ux * wy - uy * wx;
              const l = Math.hypot(nx, ny, nz) || 1; nx /= l; ny /= l; nz /= l;
              void b;
              const col = bad.has(g.s[i]) ? [0.75, 0.25, 0.2] : ny > 0.7 ? [0.28, 0.5, 0.36] : ny < -0.7 ? [0.3, 0.3, 0.42] : [0.38, 0.45, 0.62];
              const shade = 0.75 + 0.25 * Math.abs(ny);
              for (let k = 0; k < 3; k++) c.set([col[0] * shade, col[1] * shade, col[2] * shade], i * 9 + k * 3);
            }
            const mg = new THREE.BufferGeometry();
            mg.setAttribute('position', new THREE.BufferAttribute(p, 3));
            mg.setAttribute('color', new THREE.BufferAttribute(c, 3));
            mg.computeBoundingSphere();
            const solid = cfg.current.geom === 'solid';
            mesh = new THREE.Mesh(mg, new THREE.MeshBasicMaterial({ vertexColors: true, transparent: true, opacity: 0.45, side: THREE.DoubleSide, wireframe: !solid, depthWrite: false }));
            mesh.visible = cfg.current.geom !== 'off';
            scene.add(mesh);
          } catch (e) { if (!dead) setErr(`level geometry: ${e.message || e}`); }
        }

        function rebuild() {
          const { mode: m, zone: z } = cfg.current;
          const zk = z === 'auto' ? (hub.n ? `${hub.level[(hub.n - 1) % CAP]}:${hub.area[(hub.n - 1) % CAP]}` : null) : z;
          const total = hub.count();
          let k = 0, fmin = Infinity, fmax = 0;
          for (let i = 0; i < total; i++) { if (`${hub.level[i]}:${hub.area[i]}` === zk) { if (hub.frame[i] < fmin) fmin = hub.frame[i]; if (hub.frame[i] > fmax) fmax = hub.frame[i]; } }
          for (let i = 0; i < total; i++) {
            if (`${hub.level[i]}:${hub.area[i]}` !== zk) continue;
            poss[k * 3] = hub.pos[i * 3]; poss[k * 3 + 1] = hub.pos[i * 3 + 1]; poss[k * 3 + 2] = hub.pos[i * 3 + 2];
            let c;
            if (m === 'stars') c = palette.stars[hub.stars[i] % palette.stars.length];
            else if (m === 'frame') c = palette.viridis[Math.round(((hub.frame[i] - fmin) / Math.max(1, fmax - fmin)) * 63)];
            else if (m === 'worker') { tmp.set(workerColor(hub.worker[i])); c = tmp; }
            else c = hub.flags[i] & 1 ? palette.stars[3] : palette.stars[0];
            cols[k * 3] = c.r; cols[k * 3 + 1] = c.g; cols[k * 3 + 2] = c.b;
            slotOf[k] = i;
            k++;
          }
          shown = k;
          geo.setDrawRange(0, k);
          geo.attributes.position.needsUpdate = true;
          geo.attributes.color.needsUpdate = true;
          geo.boundingSphere = null;
          if (zk !== fitted && k > 0) {
            fitted = zk;
            const box3 = new THREE.Box3();
            for (let q = 0; q < k; q++) box3.expandByPoint(new THREE.Vector3(poss[q * 3], poss[q * 3 + 1], poss[q * 3 + 2]));
            const c = box3.getCenter(new THREE.Vector3());
            const r = Math.max(800, box3.getSize(new THREE.Vector3()).length() / 2);
            controls.target.copy(c);
            camera.position.set(c.x + r * 0.9, c.y + r * 1.1, c.z + r * 1.4);
          }
          loadGeometry(zk);
        }

        function select(slot) {
          if (slot == null) { marker.visible = false; chainLine.visible = false; return; }
          marker.position.set(hub.pos[slot * 3], hub.pos[slot * 3 + 1], hub.pos[slot * 3 + 2]);
          marker.visible = true;
          const ch = [slot, ...hub.chain(slot)];
          const arr = new Float32Array(ch.length * 3);
          ch.forEach((s, i) => arr.set([hub.pos[s * 3], hub.pos[s * 3 + 1], hub.pos[s * 3 + 2]], i * 3));
          chainGeo.setAttribute('position', new THREE.BufferAttribute(arr, 3));
          chainLine.visible = ch.length > 1;
        }

        const onClick = (ev) => {
          const r = renderer.domElement.getBoundingClientRect();
          mouse.set(((ev.clientX - r.left) / r.width) * 2 - 1, -((ev.clientY - r.top) / r.height) * 2 + 1);
          ray.setFromCamera(mouse, camera);
          ray.params.Points.threshold = Math.max(40, camera.position.distanceTo(controls.target) * 0.012);
          const hits = ray.intersectObject(points).filter((h) => h.index < shown);
          if (!hits.length) { hub.selected = null; setSel(null); select(null); return; }
          // The nearest to the camera among hits near the ray.
          const h = hits.reduce((a, b) => (b.distanceToRay < a.distanceToRay - 1e-6 ? b : a));
          const slot = slotOf[h.index];
          hub.selected = { slot };
          setSel(snapshot(slot));
          select(slot);
        };
        const snapshot = (slot) => ({ slot, w: hub.worker[slot], id: hub.nodeId[slot], step: hub.stepOf[slot], frame: hub.frame[slot], stars: hub.stars[slot], level: hub.level[slot], area: hub.area[slot], pos: [hub.pos[slot * 3], hub.pos[slot * 3 + 1], hub.pos[slot * 3 + 2]], progress: hub.flags[slot] & 1, chain: hub.chain(slot).length });
        let downAt = null;
        renderer.domElement.addEventListener('pointerdown', (e) => { downAt = [e.clientX, e.clientY]; });
        renderer.domElement.addEventListener('pointerup', (e) => { if (downAt && Math.hypot(e.clientX - downAt[0], e.clientY - downAt[1]) < 4) onClick(e); downAt = null; });

        const resize = () => {
          const w = el.clientWidth, h = Math.max(260, Math.round(w * 0.62));
          renderer.setSize(w, h);
          camera.aspect = w / h;
          camera.updateProjectionMatrix();
        };
        resize();
        const ro = new ResizeObserver(resize);
        ro.observe(el);
        gl.current = { reset: () => { fitted = null; rebuild(); }, setGeom: (g) => { if (mesh) { mesh.visible = g !== 'off'; mesh.material.wireframe = g === 'wire'; } } };
        let lastBuild = 0, lastHl = null;
        const frame = () => {
          raf = requestAnimationFrame(frame);
          if (!vis.current) return;
          const cf = JSON.stringify(cfg.current);
          const now = performance.now();
          if ((hub.cloudChanged || cf !== builtCfg) && now - lastBuild > 400) {
            hub.cloudChanged = false; builtCfg = cf; lastBuild = now; rebuild();
          }
          // The node of the highlight clip being played is marked like a clicked one.
          if (hub.hl !== lastHl) {
            lastHl = hub.hl;
            const hs = lastHl ? hub.slotOf(lastHl.w, lastHl.step, lastHl.id) : -1;
            if (hs >= 0) select(hs);
          }
          controls.update();
          renderer.render(scene, camera);
        };
        frame();
        cleanup = () => {
          cancelAnimationFrame(raf);
          ro.disconnect();
          controls.dispose();
          geo.dispose(); mat.dispose();
          if (mesh) { mesh.geometry.dispose(); mesh.material.dispose(); }
          renderer.dispose();
          renderer.forceContextLoss?.();
          renderer.domElement.remove();
          gl.current = null;
        };
      } catch (e) { setErr(String(e.message || e)); }
    })();
    return () => { dead = true; cleanup(); };
  }, []);
  useEffect(() => { gl.current?.setGeom(geom); }, [geom]);

  const w = sel ? hub.workers[sel.w] : null;
  const isBest = sel && w?.best && w.best.id === sel.id;
  void v;
  return (
    <Panel className="viz-cloud" innerRef={box} open={open} onToggle={() => patch({ open: !open })}
      title="Search cloud" chip={<span className="muted small">{hub.n.toLocaleString()} nodes</span>}
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
          <span className="lab">Level geometry</span>
          <div className="viz-btns">{[['solid', 'Solid'], ['wire', 'Wireframe'], ['off', 'Off']].map(([k, l]) => <button type="button" key={k} className={geom === k ? 'on' : ''} onClick={() => setGeom(k)}>{l}</button>)}</div>
          <span className="lab">View</span>
          <div className="viz-btns"><button type="button" onClick={() => gl.current?.reset()}>Fit to the nodes</button></div>
          {sel && (
            <>
              <span className="lab">Selected node</span>
              <div>
                <dl className="viz-dl" style={{ marginTop: 0 }}>
                  <dt>Node</dt><dd>{sel.id} of worker {sel.w + 1}</dd>
                  <dt>Where</dt><dd>{levelIcon(sel.level)} {levelName(sel.level)} (area {sel.area})</dd>
                  <dt>Stars</dt><dd>{sel.stars} ★{sel.progress ? ' · made progress' : ''}</dd>
                  <dt>Frame</dt><dd>{sel.frame.toLocaleString()} ({clock(sel.frame)})</dd>
                  <dt>Position</dt><dd>({sel.pos.map((x) => Math.round(x)).join(', ')})</dd>
                  <dt>Ancestors</dt><dd>{sel.chain} sampled</dd>
                </dl>
                <div className="viz-toolbar">
                  <button type="button" onClick={() => onPlay && onPlay(sel.w, { step: sel.step, id: sel.id })}>▶ Play {isBest ? 'this path' : 'this node'}</button>
                  {!isBest && w?.best && <button type="button" onClick={() => onPlay && onPlay(sel.w)}>Play its best path</button>}
                </div>
              </div>
            </>
          )}
        </form>
      )}>
      <div className="viz-glwrap">
        <div ref={mount} className="viz-gl" />
        {sel && <button className="viz-playicon" aria-label="Play this node in the game" title="Play this node in the game" onClick={() => onPlay && onPlay(sel.w, { step: sel.step, id: sel.id })}>▶</button>}
      </div>
      {err && <p className="error small">{err}</p>}
      <div className="viz-status">
        {sel
          ? <span>worker {sel.w + 1} · {levelName(sel.level)} · frame {sel.frame.toLocaleString()}</span>
          : <span className="muted">Tap a node to select it.</span>}
      </div>
    </Panel>
  );
}
