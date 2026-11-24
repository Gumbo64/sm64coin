# Browser wallet engine

`scripts/build-wallet-wasm.sh` builds the fork's wallet2 to WebAssembly (via monero-ts' emscripten
build) into `web/public/wallet/` (git-ignored, like the other wasm). The Wallet page
(`web/src/pages/Wallet.jsx`, `web/src/lib/wallet/`) fetches it at runtime and says so when it is missing.

* `sm64coin_wasm.cpp`: `sm64coin::proof_id`, the only part of the daemon's proof-of-play code a wallet
  needs (block ids commit to the proof). Mirrors `pow/sm64pow/src/{lib,proof,statecodec}.rs`; keep in step.
  Codec 2 (xz) is decoded with the vendored xz-embedded (`xz-embedded/`, see its README), as Rust's xz2 does.
* `monero-ts-sm64.patch`: build fixes for current emscripten/clang and the extra webpack configs.

CI: `.github/workflows/wallet-engine.yml` (called by pages.yml and the release workflow's website job) builds it
with emscripten 6.0.13, caches `web/wallet-wasm/work` (keyed on the script, patch, cpp and the fork's commit) and
uploads the artifact `wallet-engine`; the site jobs download it into `web/public/wallet/` before `npm run build`.
A failed engine build never fails the site: the Wallet page then says the engine is unavailable.
Needs the `monero` submodule (the script also inits its `external/rapidjson`). Verified from a fresh clone.

Tests (`web/wallet-wasm/test/`): `run.sh` compiles `sm64coin_wasm.cpp` natively (asan/ubsan) and with emscripten and checks
`proof_id` against `vectors.jsonl`, ids computed by the Rust crate (`pow/sm64pow/examples/proofid_vectors.rs`, fed by
`make-vectors.py` with real chain (the committed file has 3 blocks)'s proofs re-encoded as raw / run-length / xz, plus malformed blobs: truncated,
bit-flipped, trailing bytes, extra streams, over-long, wrong frame counts ...). `sync-check.mjs` syncs the built engine
against a node in headless Chromium.
