# Releasing

1. Make sure `main` is what you want to ship and CI is green.
2. `git tag v0.1.0 && git push origin v0.1.0` (tag names must start with `v`).
3. The `Release` workflow builds four bundles, writes `SHA256SUMS` and creates a **draft** release
   with `packaging/release-notes.md` as its text. Edit the notes, publish.

   | platform | runner | bundle |
   |---|---|---|
   | Linux x86_64 | `ubuntu-22.04` | `sm64coin-<v>-linux-x86_64.tar.gz` |
   | Linux aarch64 (Raspberry Pi 4/5) | `ubuntu-22.04-arm` | `sm64coin-<v>-linux-aarch64.tar.gz` |
   | Windows x86_64 | `windows-2022` (MSYS2 UCRT64) | `sm64coin-<v>-windows-x86_64.zip` |
   | macOS Apple silicon | `macos-15` (Homebrew) | `sm64coin-<v>-macos-arm64.tar.gz` |

   The first run of each takes 1-3 hours (Monero's C++ plus Qt); ccache and the cargo caches make
   later runs shorter. Linux is built on Ubuntu 22.04 (glibc 2.35) so the bundles also run on
   Raspberry Pi OS Bookworm (glibc 2.36), Debian 12 and newer.
4. Dry run: Actions -> Release -> Run workflow, **or** push any commit to the branch `release-test`
   (`git push origin HEAD:release-test`): builds everything and smoke-tests it, publishes nothing
   (bundles are workflow artifacts; the website is replaced by a placeholder page if its job fails).
   Only a `v*` tag runs the `publish` job. Never force-push `main`.

Repository settings needed: none (the workflow uses the built-in `GITHUB_TOKEN`; Actions needs
"Read and write permissions" under Settings -> Actions -> General). Optional secret `ROM_URL`: a
download URL of a US `baserom.us.z64` (private storage); when present every build also runs the
launcher with the ROM for 60 s (node starts, website and `/json_rpc` proxy answer).

## Reading a failed CI run without logs access

Job logs of a public repository need a login, but the build steps run through `scripts/ci-run.sh`,
which turns the end of a failed step's log and its error-looking lines into annotations:

    curl -s https://api.github.com/repos/Gumbo64/sm64coin/actions/runs?branch=release-test
    curl -s https://api.github.com/repos/Gumbo64/sm64coin/actions/runs/<run>/jobs     # step conclusions
    curl -s https://api.github.com/repos/Gumbo64/sm64coin/check-runs/<job id>/annotations

## What the bundle is

`scripts/package.sh` (the workflow runs it; run it by hand after a build) lays out one folder:
the launcher `sm64coin`, `sm64coind`, wallet CLI/RPC, the wallet app, the leaderboard server, the
website, `seeds.txt`, `README.txt` (the user's instructions, `packaging/README.txt`), and the
libraries the programs need so users install nothing:

- Linux: `lib/` and `qt/` (shared libraries and Qt plugins, found through RPATH `$ORIGIN/lib`).
- Windows: the Qt DLLs/plugins/QML from `windeployqt` and every MSYS2 DLL next to the `.exe` files.
- macOS: `SM64Coin-Wallet.app` (`macdeployqt`) and `lib/` (Homebrew dylibs of the command-line
  programs via `dylibbundler`); everything ad-hoc code-signed, **not** notarized: users do
  right-click > Open or `xattr -dr com.apple.quarantine .` (written in `README.txt`).

The ROM is never shipped. Daemon, wallet CLI/RPC and the wallet app come from ONE CMake tree
(`gui/build`: the wallet app's tree contains the daemon and libwallet, `gui/SM64COIN.md`), built with
`ARCH=default` (never `-march=native`: the bundle must run on other CPUs than the runner's).
`sm64pow` (Rust) is built by the daemon's CMake with `cargo` from the same toolchain family as the C++
compiler (Windows: MSYS2's `rust`, target `x86_64-pc-windows-gnu`).

`scripts/ci-smoke.sh` is run on every unpacked bundle: the launcher reports the missing ROM and exits
2, `sm64coind --version`, wallet CLI/RPC `--version`, the wallet app resolves its libraries (Linux
`ldd`, Windows Qt files present, macOS `otool`), the QML loads headless (informational), and with the
ROM a 60 s full run. Without the ROM it still runs the launcher against a stand-in node
(`SM64COIN_TEST_NO_ROM=1`, CI only): website, `/json_rpc` proxy, single-page routes, the wallet app as a
child process (Windows, macOS) and that ending the launcher leaves no wallet process behind.

## Known gaps

- Not signed: Windows SmartScreen and macOS Gatekeeper warn (documented in `README.txt`).
  Notarization needs an Apple developer account; Windows signing needs a certificate.
- macOS Intel and Windows ARM are not built.
- **sm64coin-automine** is not in the bundle: `pow/miner` depends on the private research crates in
  Gumbo64/sm64-AI-binds (`objective-rrt-v2`, not pushed). Once those are published (or the miner
  stops needing them) drop the `miner`/`sdk` removal in `scripts/ci-prepare.sh`.
- The website gets the shared replay player from the `sm64-binds-frontend` npm package (>= 2.2.0,
  `sm64-binds-frontend/replay`); no sibling checkouts (`ci-prepare.sh --web` is a no-op).
