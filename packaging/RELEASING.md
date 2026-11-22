# Releasing

1. Make sure `main` is what you want to ship and CI is green.
2. `git tag v0.1.0 && git push origin v0.1.0` (tag names must start with `v`).
3. The `Release` workflow builds the Linux x86_64 and aarch64 bundles (about an hour, the first
   time; ccache and the cargo cache make later runs shorter), writes `SHA256SUMS` and creates a
   **draft** release with `packaging/release-notes.md` as its text. Edit the notes, publish.
4. Dry run: Actions -> Release -> Run workflow: builds everything, publishes nothing (bundles are
   workflow artifacts).

Repository settings needed: none (the workflow uses the built-in `GITHUB_TOKEN`; Actions needs
"Read and write permissions" under Settings -> Actions -> General). Optional secret `ROM_URL` for CI
tests that need the ROM.

## What the bundle is

`scripts/package.sh` (the workflow runs it; run it by hand after `make build`) lays out one folder:
the launcher `sm64coin`, `sm64coind`, wallet CLI/RPC, the wallet app, the leaderboard server, the
website, `seeds.txt`, and (Linux) `lib/` and `qt/` with the shared libraries and Qt plugins, found
through RPATH. The ROM is never shipped.

## Known gaps

- **Windows and macOS** are not built. Upstream's MSYS2 (Windows) and macOS jobs in
  `monero/.github/workflows/build.yml` and `gui/.github/workflows/build.yml` are the starting point;
  the launcher itself is written to be cross-platform (not yet compiled for them), and
  `package.sh` already names `windows-*` / `macos-*` platforms (zip, no library bundling).
- **sm64coin-automine** is not in the bundle: `pow/miner` depends on the private research crates in
  Gumbo64/sm64-AI-binds (`objective-rrt-v2`, not pushed). Once those are published (or the miner
  stops needing them) drop the `miner`/`sdk` removal in `scripts/ci-prepare.sh`.
- `scripts/ci-prepare.sh --web` checks out sm64-viewer and sm64-binds next to the repo for the
  shared replay player; remove it when the player ships in the sm64-binds npm package.
- `sm64-binds` comes from git (tag v2.1.1) via `[patch.crates-io]` in `pow/Cargo.toml` until it is on
  crates.io; delete that section then.
