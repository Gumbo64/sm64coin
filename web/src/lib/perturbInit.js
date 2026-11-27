// Loading of the perturbation WASM (pow/perturb-wasm), shared by lib/perturb.js (the controller perturbation) and lib/limit.js (the
// time limit from the Rust rules). One instance per page: wasm-bindgen's init() twice at once would make two.
import init from '../wasm/perturb/sm64coin_perturb_wasm.js';

let ready = null;
/** Loads the module once. `src` (tests under node): the .wasm bytes, since fetch cannot read a file URL there. */
export const perturbReady = (src) => (ready ??= init(src ? { module_or_path: src } : undefined));
