# Browser wallet engine

`scripts/build-wallet-wasm.sh` builds the fork's wallet2 to WebAssembly (via monero-ts' emscripten
build) into `web/public/wallet/` (git-ignored, like the other wasm). The Wallet page
(`web/src/pages/Wallet.jsx`, `web/src/lib/wallet/`) fetches it at runtime and says so when it is missing.

* `sm64coin_wasm.cpp`: `sm64coin::proof_id`, the only part of the daemon's proof-of-play code a wallet
  needs (block ids commit to the proof). Mirrors `pow/sm64pow/src/{lib,proof,statecodec}.rs`; keep in step.
  Codec 2 (xz) needs liblzma and is not supported (such a block would fail to hash).
* `monero-ts-sm64.patch`: build fixes for current emscripten/clang and the extra webpack configs.
