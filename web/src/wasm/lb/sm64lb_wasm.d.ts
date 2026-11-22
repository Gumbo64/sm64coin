/* tslint:disable */
/* eslint-disable */

/**
 * The hex of the tag record for `address` (checks the view key belongs to it); errors say why not.
 */
export function encode_tag(name: string, address: string, view_key_hex: string): string;

/**
 * Is any extra's nonce a tag candidate (cheap pre-check before the elliptic-curve work)?
 */
export function has_tag(extra: Uint8Array | number[]): boolean;

/**
 * The coinbase tag check. `keys` is the outputs' one-time keys back to back (32 bytes each),
 * `view_tags` one byte per output (or empty when the outputs carry none), `amounts` the
 * atomic amounts. Returns "" (no tag, or it does not verify) or
 * `{"name":..,"address":..,"reward":N,"outputs":[..]}`.
 */
export function verify_tagged_coinbase(extra: Uint8Array | number[], keys: Uint8Array | number[], view_tags: Uint8Array | number[], amounts: Float64Array | number[]): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly encode_tag: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly has_tag: (a: number, b: number) => number;
    readonly verify_tagged_coinbase: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number];
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
