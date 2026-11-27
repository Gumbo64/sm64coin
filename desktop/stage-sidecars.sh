#!/usr/bin/env bash
# Stage the release bundle's programs for the Tauri bundler: desktop/src-tauri/stage/
#   binaries/<name>-<target triple>[.exe]   bundle.externalBin: sm64coind, sm64coin-wallet-rpc, sm64coin-automine, rrtrun
#   lib/        Linux/macOS: only the shared libraries those four programs need (RUNPATH $ORIGIN/lib, @executable_path/lib), placed
#               next to the installed programs by the per-platform bundle config (src-tauri/bundle/*.json)
#   dll/        Windows: the DLLs next to the .exe files in the bundle, minus Qt (the wallet GUI is not part of the app)
#   res/        seeds.txt, routes/
# usage: stage-sidecars.sh <unpacked release bundle dir> [target triple]   (triple: rustc -vV host by default)
set -euo pipefail
SRC="$(cd "${1:?usage: stage-sidecars.sh <unpacked release bundle> [triple]}" && pwd)"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TRIPLE="${2:-$(rustc -vV | sed -n 's/^host: //p')}"
OUT="$HERE/src-tauri/stage"
rm -rf "$OUT"; mkdir -p "$OUT/binaries" "$OUT/res"
X=""; case "$TRIPLE" in *windows*) X=".exe" ;; esac
for n in sm64coind sm64coin-wallet-rpc sm64coin-automine rrtrun; do
  [ -f "$SRC/$n$X" ] || { echo "stage-sidecars: $SRC/$n$X is missing" >&2; exit 1; }
  cp -L "$SRC/$n$X" "$OUT/binaries/$n-$TRIPLE$X"; chmod +x "$OUT/binaries/$n-$TRIPLE$X"
done
cp "$SRC/seeds.txt" "$OUT/res/seeds.txt"
mkdir -p "$OUT/res/routes"; cp "$SRC"/routes/*.m64 "$OUT/res/routes/"
case "$TRIPLE" in
  *linux*|*darwin*)
    mkdir -p "$OUT/lib"
    if [ -d "$SRC/lib" ]; then
      # Keep what the four programs (and, in a second pass, those libraries) import from the bundle's lib/; leave out Qt and the rest.
      need=""; todo=""
      for n in sm64coind sm64coin-wallet-rpc sm64coin-automine rrtrun; do todo="$todo $SRC/$n"; done
      for pass in 1 2 3 4; do
        for f in $todo; do
          case "$TRIPLE" in
            *linux*) deps="$(LD_LIBRARY_PATH= ldd "$f" 2>/dev/null | awk '/=> \//{print $3}' | grep "^$SRC/lib/" || true)" ;;
            *) deps="$(otool -L "$f" 2>/dev/null | awk '{print $1}' | sed -n "s#^@executable_path/lib/#$SRC/lib/#p" | while read -r p; do [ -e "$p" ] && echo "$p"; done || true)" ;;
          esac
          need="$need $deps"
        done
        todo="$(echo $need | tr ' ' '\n' | sort -u | tr '\n' ' ')"
      done
      for l in $todo; do cp -L "$l" "$OUT/lib/$(basename "$l")"; done
    fi
    echo "lib: $(ls "$OUT/lib" | wc -l) files, $(du -sh "$OUT/lib" | cut -f1)" ;;
  *windows*)
    mkdir -p "$OUT/dll"
    for d in "$SRC"/*.dll; do
      case "$(basename "$d")" in Qt5*|qt*|libqt*) continue ;; esac
      cp "$d" "$OUT/dll/"
    done
    echo "dll: $(ls "$OUT/dll" | wc -l) files" ;;
esac
echo "staged in $OUT for $TRIPLE"
