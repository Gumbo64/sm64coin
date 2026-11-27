// The platform of a normal browser tab: no native parts. The page talks to nodes over fetch (lib/rpc.js) and mines with
// its WASM workers.
export const browser = {
  name: 'browser',
  desktop: false,
  rpc: null, // lib/rpc.js uses fetch
  nodeUrl: () => '',
  node: null,
  nativeMiner: null,
  rom: null,
  wallet: null, // the Wallet page uses the WASM engine (lib/wallet/engine.js)
  host: null,
};
