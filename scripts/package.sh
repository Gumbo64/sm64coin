#!/usr/bin/env bash
# Build the release bundle of the already built binaries: dist/sm64coin-<VERSION>-<PLATFORM>.tar.gz
# (.zip when PLATFORM is windows-*). The release workflow runs exactly this.
#
# Bundle layout (flat, everything next to the wallet app):
#   sm64coind  sm64coin-node (+ .bat on Windows, sm64coin-node.service on Linux)  sm64coin-wallet-cli  sm64coin-wallet-rpc  sm64coin-wallet-gui
#   sm64coin-leaderboard  [sm64coin-automine]  web/  seeds.txt  README.txt  LICENSE
#   Linux:   lib/ qt/ qt.conf   the shared libraries and Qt plugins the binaries need, found
#                               through RPATH $ORIGIN/lib, so users install nothing
#   Windows: the GUI's Qt DLLs/plugins/qml (windeployqt, run before this script) and every
#            MSYS2 DLL the programs import, all next to the .exe files; zip
#   macOS:   SM64Coin-Wallet.app (macdeployqt) instead of sm64coin-wallet-gui, lib/ with the
#            Homebrew dylibs of the command-line programs (dylibbundler), ad-hoc code-signed
#
# Env (all optional):
#   VERSION   (git describe)        PLATFORM (linux-x86_64 / linux-aarch64 from uname)
#   OUT       (dist)                MONERO_BIN (monero/build/release/bin: monerod, monero-wallet-cli/-rpc)
#   GUI_BIN   (gui/build/bin/monero-wallet-gui; skipped if missing)
#   GUI_APP   (macOS: gui/build/bin/monero-wallet-gui.app, the deployed app bundle)
#   POW_TARGET (pow/target/release: sm64coin-leaderboard, automine and its rrtrun; each skipped if missing)
#   ALLOW_NO_WEB=1  dry runs: a placeholder page when there is no website build
#   WEB_DIST (web/dist)
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
VERSION="${VERSION:-$(git describe --tags --always --dirty 2>/dev/null || echo dev)}"
VERSION="${VERSION#v}"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) def_plat=linux-x86_64 ;;
  Linux-aarch64|Linux-arm64) def_plat=linux-aarch64 ;;
  Darwin-arm64) def_plat=macos-arm64 ;;
  Darwin-x86_64) def_plat=macos-x86_64 ;;
  *) def_plat=windows-x86_64 ;;
esac
PLATFORM="${PLATFORM:-$def_plat}"
OUT="${OUT:-$ROOT/dist}"
MONERO_BIN="${MONERO_BIN:-$ROOT/monero/build/release/bin}"
GUI_BIN="${GUI_BIN:-$ROOT/gui/build/bin/monero-wallet-gui}"
GUI_APP="${GUI_APP:-$ROOT/gui/build/bin/monero-wallet-gui.app}"
POW_TARGET="${POW_TARGET:-$ROOT/pow/target/release}"
WEB_DIST="${WEB_DIST:-$ROOT/web/dist}"
X=""; case "$PLATFORM" in windows-*) X=".exe" ;; esac

NAME="sm64coin-$VERSION-$PLATFORM"
STAGE="$OUT/$NAME"
rm -rf "$STAGE"; mkdir -p "$STAGE"

need() { [ -e "$1" ] || { echo "package.sh: missing $1" >&2; exit 1; }; }
put() { # put <src> <dest name> [required]
  if [ -e "$1" ]; then cp -L "$1" "$STAGE/$2"; chmod +x "$STAGE/$2"; echo "  + $2"
  elif [ "${3:-}" = required ]; then echo "package.sh: missing $1" >&2; exit 1
  else echo "  - $2 skipped (no $1)"; fi
}
echo "Packaging $NAME"
put "$MONERO_BIN/monerod$X"                "sm64coind$X" required
put "$MONERO_BIN/monero-wallet-cli$X"      "sm64coin-wallet-cli$X" required
put "$MONERO_BIN/monero-wallet-rpc$X"      "sm64coin-wallet-rpc$X" required
case "$PLATFORM" in
  macos-*) [ -d "$GUI_APP" ] && { cp -R "$GUI_APP" "$STAGE/SM64Coin-Wallet.app"; echo "  + SM64Coin-Wallet.app"; } || echo "  - wallet app skipped (no $GUI_APP)" ;;
  *) put "$GUI_BIN$X"                        "sm64coin-wallet-gui$X" ;;
esac
# The node script for servers (no wallet app): a shell script, or a .bat on Windows.
case "$PLATFORM" in
  windows-*) put "$ROOT/packaging/sm64coin-node.bat" "sm64coin-node.bat" required
             sed -i 's/\r$//; s/$/\r/' "$STAGE/sm64coin-node.bat" ;;   # cmd.exe wants CRLF
  *) put "$ROOT/scripts/sm64coin-node" "sm64coin-node" required ;;
esac
case "$PLATFORM" in linux-*) cp "$ROOT/packaging/sm64coin-node.service" "$STAGE/sm64coin-node.service"; echo "  + sm64coin-node.service" ;; esac
put "$POW_TARGET/sm64coin-leaderboard$X"   "sm64coin-leaderboard$X"
put "$POW_TARGET/automine$X"               "sm64coin-automine$X"
put "$POW_TARGET/rrtrun$X"                 "rrtrun$X"   # the miner's tree-search child, found next to it
if [ ! -f "$WEB_DIST/index.html" ] && [ -n "${ALLOW_NO_WEB:-}" ]; then   # dry runs only
  echo "package.sh: WARNING no website build, using a placeholder page"
  mkdir -p "$WEB_DIST"; echo '<!doctype html><title>SM64Coin</title><p>placeholder website (dry run)</p>' > "$WEB_DIST/index.html"
fi
need "$WEB_DIST/index.html"
cp -r "$WEB_DIST" "$STAGE/web"
cp seeds.txt "$STAGE/seeds.txt"
sed "s/@VERSION@/$VERSION/g" packaging/README.txt > "$STAGE/README.txt"
case "$PLATFORM" in windows-*) sed -i 's/$/\r/' "$STAGE/README.txt" ;; esac   # Notepad-friendly
[ -f monero/LICENSE ] && cp monero/LICENSE "$STAGE/LICENSE"
[ -d pow/miner/routes ] && [ -e "$STAGE/sm64coin-automine$X" ] && { mkdir -p "$STAGE/routes"; cp pow/miner/routes/block1-human.m64 "$STAGE/routes/"; }

# ---- Linux: carry the shared libraries ---------------------------------------------------
bundle_linux_libs() {
  command -v patchelf >/dev/null || { echo "package.sh: patchelf is required (apt install patchelf)" >&2; exit 1; }
  # Not bundled: the C library family, GPU/display drivers (must match the machine) and the
  # C++ runtime (the machine's is newer than ours, and the drivers need that one).
  local skip='^(linux-vdso|ld-linux|libc|libm|libdl|libpthread|librt|libutil|libresolv|libnsl|libanl|libstdc\+\+|libgcc_s|libGL|libGLX|libGLdispatch|libEGL|libOpenGL|libdrm|libgbm|libwayland-[a-z]+|libvulkan)[.-]'
  mkdir -p "$STAGE/lib"
  local gui="$STAGE/sm64coin-wallet-gui" elfs=()
  if [ -f "$gui" ]; then
    local qplug qqml
    qplug="$(qmake -query QT_INSTALL_PLUGINS 2>/dev/null || qmake-qt5 -query QT_INSTALL_PLUGINS)"
    qqml="$(qmake -query QT_INSTALL_QML 2>/dev/null || qmake-qt5 -query QT_INSTALL_QML)"
    mkdir -p "$STAGE/qt/plugins"
    for d in imageformats iconengines platforminputcontexts xcbglintegrations; do
      [ -d "$qplug/$d" ] && cp -r "$qplug/$d" "$STAGE/qt/plugins/"
    done
    mkdir -p "$STAGE/qt/plugins/platforms"   # xcb (X11/XWayland); offscreen for headless runs
    cp "$qplug"/platforms/libqxcb.so "$qplug"/platforms/libqoffscreen.so "$STAGE/qt/plugins/platforms/"
    cp -r "$qqml" "$STAGE/qt/qml"
    rm -rf "$STAGE/qt/qml/QtMultimedia"      # only the QR scanner page, which is not built
    printf '[Paths]\nPrefix=.\nPlugins=qt/plugins\nQml2Imports=qt/qml\nLibraries=lib\n' > "$STAGE/qt.conf"
  fi
  while IFS= read -r f; do
    [ "$(head -c4 "$f" | tail -c3)" = ELF ] && elfs+=("$f")
  done < <(find "$STAGE" -type f \( -perm -u+x -o -name '*.so*' \) -not -path "$STAGE/lib/*")
  # Collect dependencies (ldd lists the whole closure); a library's own dependencies are
  # added in a second pass.
  local pass libs
  for pass in 1 2; do
    libs="$( { for f in "${elfs[@]}" "$STAGE"/lib/*.so*; do if [ -e "$f" ]; then ldd "$f" 2>/dev/null || true; fi; done; } \
      | awk '/=> \//{print $3}' | sort -u)"
    for l in $libs; do
      local b; b="$(basename "$l")"
      if [[ ! "$b" =~ $skip ]] && [ ! -e "$STAGE/lib/$b" ]; then cp -L "$l" "$STAGE/lib/$b"; fi
    done
  done
  for f in "${elfs[@]}" "$STAGE"/lib/*.so*; do
    local rel; rel="$(realpath --relative-to="$(dirname "$f")" "$STAGE/lib")"
    patchelf --set-rpath "\$ORIGIN/$rel" "$f" 2>/dev/null || true
  done
  # Missing-library check: nothing may resolve outside the bundle except the skipped family.
  local bad
  bad="$(for f in "${elfs[@]}"; do LD_LIBRARY_PATH= ldd "$f" 2>/dev/null | grep 'not found' || true; done | sort -u)"
  [ -z "$bad" ] || { echo "package.sh: unresolved libraries:" >&2; echo "$bad" >&2; exit 1; }
  strip --strip-unneeded "${elfs[@]}" 2>/dev/null || true
  du -sh "$STAGE/lib" | sed 's/^/  lib: /'
}
# ---- Windows: the GUI's deployed Qt files and every MSYS2 DLL ----------------------------------
bundle_windows_libs() {
  local gdir; gdir="$(dirname "$GUI_BIN")"
  if [ -f "$GUI_BIN$X" ]; then
    # windeployqt output: DLLs and plugin/qml folders next to the GUI exe (the exes are put above).
    for e in "$gdir"/*; do
      case "$e" in *.exe|*.a|*.lib|*.log) continue ;; esac
      [ "$(basename "$e")" = "monero" ] && continue
      cp -R "$e" "$STAGE/"
    done
  fi
  local pref="${MSYSTEM_PREFIX:-/ucrt64}" pass f l
  for pass in 1 2 3; do
    while IFS= read -r f; do ldd "$f" 2>/dev/null || true; done < <(find "$STAGE" -type f \( -name '*.exe' -o -name '*.dll' \)) \
      | awk -v p="$pref/" 'index($3,p)==1 {print $3}' | sort -u | while IFS= read -r l; do cp -n "$l" "$STAGE/"; done
  done
  # Nothing may still import an MSYS2 DLL that is not next to the programs.
  local bad
  bad="$(find "$STAGE" -type f \( -name '*.exe' -o -name '*.dll' \) -print0 | xargs -0 -n1 ldd 2>/dev/null | awk -v p="$pref/" 'index($3,p)==1 {print $1}' | sort -u | while read -r d; do [ -e "$STAGE/$d" ] || echo "$d"; done)"
  [ -z "$bad" ] || { echo "package.sh: DLLs not bundled: $bad" >&2; exit 1; }
}

# macOS binaries of the bundle that still import the build machine's Homebrew, or import an
# @executable_path / @loader_path library that is not there. (bash 3.2: keep this out of a $( )
# with parentheses in it.)
mac_bad_imports() {
  set +e +o pipefail
  local f id dep exe r
  find "$STAGE" -type f | while read -r f; do
    if file "$f" | grep -q 'Mach-O'; then
      id="$(otool -D "$f" 2>/dev/null | tail -n +2)"   # a library's own install name is not an import
      case "$f" in "$STAGE"/SM64Coin-Wallet.app/*) exe="$STAGE/SM64Coin-Wallet.app/Contents/MacOS" ;; *) exe="$STAGE" ;; esac
      for dep in $(otool -L "$f" | tail -n +2 | awk '{print $1}'); do
        [ "$dep" = "$id" ] && continue
        case "$dep" in
          /opt/homebrew/*|/usr/local/*) echo "${f#$STAGE/}: $dep" ;;
          @executable_path/*) r="$exe/${dep#@executable_path/}"; [ -e "$r" ] || echo "${f#$STAGE/}: missing $dep" ;;
          @loader_path/*) r="$(dirname "$f")/${dep#@loader_path/}"; [ -e "$r" ] || echo "${f#$STAGE/}: missing $dep" ;;
        esac
      done
    fi
  done
}

# macdeployqt leaves some libraries of the app pointing at Homebrew or at a @loader_path that
# only exists there (libbrotlidec -> libbrotlicommon): copy what is missing into the app's
# Frameworks and point everything at it.
mac_fix_app() {
  local app="$1" fw="$1/Contents/Frameworks" pass changed f id dep base src
  for pass in 1 2 3 4 5; do
    changed=0
    while read -r f; do
      file "$f" | grep -q 'Mach-O' || continue
      id="$(otool -D "$f" 2>/dev/null | tail -n +2)"
      for dep in $(otool -L "$f" | tail -n +2 | awk '{print $1}'); do
        [ "$dep" = "$id" ] && continue
        base="$(basename "$dep")"; src=""
        case "$dep" in
          /opt/homebrew/*|/usr/local/*)
            case "$dep" in
              *.framework/*) [ -e "$fw/${dep#*/lib/}" ] && install_name_tool -change "$dep" "@executable_path/../Frameworks/${dep#*/lib/}" "$f" && changed=1; continue ;;
            esac
            src="$dep" ;;
          @loader_path/*|@rpath/*)
            case "$dep" in *.framework/*) continue ;; esac
            [ -e "$(dirname "$f")/${dep#@loader_path/}" ] && continue
            [ -e "$fw/$base" ] || src="$(ls /opt/homebrew/opt/*/lib/"$base" /opt/homebrew/lib/"$base" 2>/dev/null | head -1)" ;;
          *) continue ;;
        esac
        if [ ! -e "$fw/$base" ]; then
          [ -n "$src" ] && [ -e "$src" ] || { echo "  ! ${f#$app/} needs $dep: not found anywhere"; continue; }
          cp -L "$src" "$fw/$base"; chmod u+w "$fw/$base"
          install_name_tool -id "@executable_path/../Frameworks/$base" "$fw/$base" 2>/dev/null
          echo "  + $base copied into the app (for ${f#$app/})"
        fi
        install_name_tool -change "$dep" "@executable_path/../Frameworks/$base" "$f" && changed=1
      done
    done < <(find "$app" -type f)
    [ "$changed" = 1 ] || break
  done
}

# ---- macOS: Homebrew dylibs of the command-line programs; ad-hoc signatures -----------------
bundle_macos_libs() {
  command -v dylibbundler >/dev/null || { echo "package.sh: dylibbundler is required (brew install dylibbundler)" >&2; exit 1; }
  set +e +o pipefail   # the loops below probe files with commands that may fail; errors are checked explicitly
  local args=() f
  for f in sm64coind sm64coin-wallet-cli sm64coin-wallet-rpc sm64coin-leaderboard sm64coin-automine rrtrun; do
    [ -f "$STAGE/$f" ] && args+=(-x "$STAGE/$f")
  done
  # The deployed app carries its own copy of the daemon (a post-build step of the wallet app); the
  # bundle's sm64coind replaces it, and it would import Homebrew's libzmq.
  rm -f "$STAGE/SM64Coin-Wallet.app/Contents/MacOS/monerod"
  dylibbundler -od -b "${args[@]}" -d "$STAGE/lib" -p @executable_path/lib/ -s /opt/homebrew/lib -s /usr/local/lib > "$OUT/dylibbundler.log" 2>&1 \
    || { tail -n 25 "$OUT/dylibbundler.log" >&2; exit 1; }
  [ -d "$STAGE/SM64Coin-Wallet.app" ] && mac_fix_app "$STAGE/SM64Coin-Wallet.app"
  # install_name_tool invalidated the signatures; Apple Silicon refuses unsigned code.
  find "$STAGE" -type f \( -perm -u+x -o -name '*.dylib' \) | while read -r f; do
    if file "$f" | grep -q 'Mach-O'; then codesign --force --sign - "$f" 2>/dev/null || echo "codesign failed: $f"; fi
  done
  if [ -d "$STAGE/SM64Coin-Wallet.app" ]; then codesign --force --deep --sign - "$STAGE/SM64Coin-Wallet.app" 2>/dev/null; fi
  local bad
  bad="$(mac_bad_imports)" || true
  [ -z "$bad" ] || { echo "package.sh: binaries still import Homebrew libraries:" >&2; echo "$bad" | head -40 >&2; exit 1; }
  du -sh "$STAGE/lib" | sed 's/^/  lib: /'
  set -e -o pipefail
}
case "$PLATFORM" in linux-*) bundle_linux_libs ;; windows-*) bundle_windows_libs ;; macos-*) bundle_macos_libs ;; esac

# ---- archive --------------------------------------------------------------------------------
cd "$OUT"
case "$PLATFORM" in
  windows-*) rm -f "$NAME.zip"; (command -v zip >/dev/null && zip -qr "$NAME.zip" "$NAME") || 7z a -tzip -bso0 "$NAME.zip" "$NAME"; ARCHIVE="$NAME.zip" ;;
  *) tar -czf "$NAME.tar.gz" "$NAME"; ARCHIVE="$NAME.tar.gz" ;;
esac
echo "Wrote $OUT/$ARCHIVE ($(du -h "$ARCHIVE" | cut -f1))"
