// For tests under node: loads the built perturbation WASM into lib/limit.js (false when it is not built: the caller skips).
import { existsSync, readFileSync } from 'node:fs';
import { limitReady } from './limit.js';

const wasm = new URL('../wasm/perturb/sm64coin_perturb_wasm_bg.wasm', import.meta.url);
export async function loadLimit() {
  if (!existsSync(wasm)) return false;
  await limitReady(readFileSync(wasm));
  return true;
}
