// The star board: rendered (node, react-dom/server, the JSX bundled by esbuild on the fly) from the Rust reader's vectors, and the
// WASM's own answer checked against the same vectors (pow/perturb/tests/star_board_vectors.json, made by pow/perturb's test).
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { build } from 'esbuild';
import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

const vectors = JSON.parse(readFileSync(new URL('../../../pow/perturb/tests/star_board_vectors.json', import.meta.url), 'utf8'));
const board = vectors.board;
assert.equal(board.total, 120);
assert.equal(board.got, 5);

// ---- render
const tmp = new URL('../../node_modules/.starboard-test/', import.meta.url);
mkdirSync(tmp, { recursive: true });
const out = await build({ entryPoints: [new URL('./StarBoard.jsx', import.meta.url).pathname], bundle: true, format: 'esm', platform: 'node', external: ['react', 'react-dom'], loader: { '.jsx': 'jsx' }, jsx: 'automatic', outdir: 'out', write: false });
writeFileSync(new URL('board.mjs', tmp), out.outputFiles.find((f) => f.path.endsWith('.js')).text);
const { default: StarBoard } = await import(new URL('board.mjs', tmp));
const html = renderToStaticMarkup(React.createElement(StarBoard, { board }));
rmSync(tmp, { recursive: true, force: true });
assert.match(html, /<b>5<\/b> of 120 stars · 115 left/, 'the header');
assert.equal((html.match(/class="star-row"/g) || []).length, 17, 'a row per course, the castle and the secret ones');
assert.equal((html.match(/star-cell got/g) || []).length, 5);
assert.equal((html.match(/>★</g) || []).length, 5);
assert.equal((html.match(/>☆</g) || []).length, 115);
assert.match(html, /title="Big Bob-omb on the Summit: collected"/);
assert.match(html, /title="Footrace with Koopa the Quick: not yet"/);
assert.match(html, /■ wing cap switch/);
assert.match(html, /□ metal cap switch/);
assert.match(html, /■ Bowser key 1/);
assert.match(html, /□ Bowser key 2/);
assert.match(html, /2\/7/);
assert.equal(renderToStaticMarkup(React.createElement(StarBoard, { board: null })), '', 'nothing without a board');

// ---- the WASM gives the Rust board
const wasm = new URL('../wasm/perturb/sm64coin_perturb_wasm_bg.wasm', import.meta.url);
if (existsSync(wasm)) {
  const m = await import('../wasm/perturb/sm64coin_perturb_wasm.js');
  m.initSync({ module: readFileSync(wasm) });
  const bytes = Uint8Array.from(vectors.state_hex.match(/../g).map((h) => parseInt(h, 16)));
  assert.deepEqual(JSON.parse(m.star_board(bytes)), board, 'the WASM star board equals the Rust one');
} else console.log('starBoard: wasm not built, the wasm check is skipped');
console.log('starBoard ok');
