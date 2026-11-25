# Browser wallet engine

`scripts/build-wallet-wasm.sh` builds the fork's wallet2 to WebAssembly (via monero-ts' emscripten
build) into `web/public/wallet/` (git-ignored, like the other wasm). The Wallet page
(`web/src/pages/Wallet.jsx`, `web/src/lib/wallet/`) fetches it at runtime and says so when it is missing.

* `sm64coin_wasm.cpp`: `sm64coin::proof_id`, the only part of the daemon's proof-of-play code a wallet
  needs (block ids commit to the proof). Since v0.4 the id is the 32-byte `input_hash` in the proof header (read, not
  decoded; a header that does not parse gets `keccak256("sm64bad!" || blob)`); mirrors `pow/sm64pow/src/commit.rs`; keep in step.
  `xz-embedded/` (and its line in `monero-ts-sm64.patch`) is no longer used by `proof_id`: it is left in the build, unused.
* `monero-ts-sm64.patch`: build fixes for current emscripten/clang and the extra webpack configs.

CI: `.github/workflows/wallet-engine.yml` (called by pages.yml and the release workflow's website job) builds it
with emscripten 6.0.13, caches `web/wallet-wasm/work` (keyed on the script, patch, cpp and the fork's commit) and
uploads the artifact `wallet-engine`; the site jobs download it into `web/public/wallet/` before `npm run build`.
A failed engine build never fails the site: the Wallet page then says the engine is unavailable.
Needs the `monero` submodule (the script also inits its `external/rapidjson`). Verified from a fresh clone.

Tests (`web/wallet-wasm/test/`): `run.sh` compiles `sm64coin_wasm.cpp` natively (asan/ubsan) and with emscripten and checks
`proof_id` against `vectors-v04.jsonl`, ids computed by the Rust crate (`pow/sm64pow/examples/proofid_vectors_v04.rs`: synthetic and real runs committed in
raw / run-length / xz, bad encodings that keep the id, header faults that get the `sm64bad!` id). `vectors.jsonl` are the v0.3 vectors (kept, no longer run). `sync-check.mjs` syncs the built engine
against a node in headless Chromium.
