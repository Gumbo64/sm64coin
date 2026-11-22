#!/usr/bin/env bash
# ci-smoke.sh BUNDLE_DIR : what can be checked about an unpacked release bundle without a ROM
# (and, when the ROM is given in SM64COIN_ROM_FILE, a 60 s full run). Used by release.yml.
set -uo pipefail
B="$(cd "$1" && pwd)"
cd "$B"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) OS=windows; X=.exe ;; Darwin) OS=macos; X= ;; *) OS=linux; X= ;; esac
fail=0
bad() { echo "::error title=smoke test::$*"; fail=1; }
H="$B/../smoke-home"; mkdir -p "$H"

echo "--- bundle"; ls -la | head -45; du -sh .
echo "--- launcher without a ROM: must say so and exit 2"
out="$(HOME="$H" USERPROFILE="$H" SM64COIN_ROM= ./sm64coin$X < /dev/null 2>&1)"; rc=$?
echo "$out"; echo "exit code $rc"
[ "$rc" = 2 ] || bad "sm64coin exited with $rc, expected 2"
echo "$out" | grep -q "Could not find your Super Mario 64 ROM" || bad "sm64coin did not report the missing ROM"
./sm64coin$X --version || bad "sm64coin --version"
./sm64coin$X --help | head -5 || true

echo "--- command-line programs"
for p in sm64coind sm64coin-wallet-cli sm64coin-wallet-rpc; do
  ./$p$X --version || bad "$p --version failed"
done

echo "--- wallet app libraries"
case "$OS" in
  linux)
    ldd ./sm64coin-wallet-gui | grep -i 'not found' && bad "wallet app: missing libraries"
    ldd ./sm64coind | grep -i 'not found' && bad "sm64coind: missing libraries"
    gui=./sm64coin-wallet-gui ;;
  windows)
    [ -f sm64coin-wallet-gui.exe ] || bad "wallet app missing"
    ls Qt5Core.dll Qt5Qml.dll Qt5Quick.dll platforms/qwindows.dll >/dev/null || bad "Qt files missing"
    gui=./sm64coin-wallet-gui.exe ;;
  macos)
    app=SM64Coin-Wallet.app/Contents/MacOS/monero-wallet-gui
    [ -x "$app" ] || bad "wallet app missing"
    otool -L "$app" | head -20
    ls SM64Coin-Wallet.app/Contents/Frameworks | head -30
    gui="./$app" ;;
esac
# QML loads (headless). Informational: a display-less runner may lack what Qt wants.
echo "--- wallet app --check-qml (headless, informational)"
( QT_QPA_PLATFORM=offscreen HOME="$H" USERPROFILE="$H" "$gui" --check-qml > "$H/qml.log" 2>&1 & p=$!; for i in $(seq 40); do sleep 1; kill -0 $p 2>/dev/null || break; done; kill $p 2>/dev/null; wait $p; echo "check-qml exit code $?" ) 
tail -n 15 "$H/qml.log"

if [ -n "${SM64COIN_ROM_FILE:-}" ] && [ -f "${SM64COIN_ROM_FILE}" ]; then
  echo "--- full run for 60 s with the ROM"
  cp "$SM64COIN_ROM_FILE" "$H/baserom.us.z64"
  HOME="$H" USERPROFILE="$H" ./sm64coin$X --no-browser --no-wallet --solo --data-dir "$H/data" > "$H/run.log" 2>&1 &
  pid=$!
  up=0
  for i in $(seq 60); do sleep 1; grep -q "Node is up" "$H/run.log" && up=1; kill -0 $pid 2>/dev/null || break; done
  cat "$H/run.log"
  [ "$up" = 1 ] || bad "full run: the node did not come up"
  curl -s -m 5 http://127.0.0.1:5181/ | head -c 200 | grep -qi html || bad "full run: website not served"
  curl -s -m 5 -d '{"jsonrpc":"2.0","id":0,"method":"get_info"}' http://127.0.0.1:5181/json_rpc | grep -q height || bad "full run: /json_rpc proxy"
  kill $pid 2>/dev/null; sleep 5
  [ "$OS" = windows ] && taskkill //F //IM sm64coind.exe >/dev/null 2>&1
  kill -9 $pid 2>/dev/null
else
  echo "(no ROM available: full run skipped)"
fi
exit $fail
