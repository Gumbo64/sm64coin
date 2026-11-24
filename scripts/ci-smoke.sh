#!/usr/bin/env bash
# ci-smoke.sh BUNDLE_DIR : what can be checked about an unpacked release bundle without a ROM
# (and, when the ROM is given in SM64COIN_ROM_FILE, a 60 s run of the node script). Used by release.yml.
set -uo pipefail
B="$(cd "$1" && pwd)"
cd "$B"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) OS=windows; X=.exe ;; Darwin) OS=macos; X= ;; *) OS=linux; X= ;; esac
fail=0
bad() { echo "::error title=smoke test::$*"; fail=1; }
H="$B/../smoke-home"; mkdir -p "$H"

echo "--- bundle"; ls -la | head -45; du -sh .
echo "--- node script: --help, and without a ROM it must say so and exit 2"
if [ "$OS" = windows ]; then
  [ -f sm64coin-node.bat ] || bad "sm64coin-node.bat missing"
  cmd //c "sm64coin-node.bat --help" | head -5 || bad "sm64coin-node.bat --help"
  out="$(HOME="$H" USERPROFILE="$H" ROM= cmd //c "sm64coin-node.bat" < /dev/null 2>&1)"; rc=$?
else
  [ -x ./sm64coin-node ] || bad "sm64coin-node missing"
  ./sm64coin-node --help | head -5 || bad "sm64coin-node --help"
  # (not in /opt or next to a ROM: HERE/.. is searched too, so the bundle's parent must not hold one)
  out="$(HOME="$H" ROM= SM64COIN_ROM= ./sm64coin-node --data-dir "$H/nodescript" < /dev/null 2>&1)"; rc=$?
fi
echo "$out"; echo "exit code $rc"
[ "$rc" = 2 ] || bad "sm64coin-node exited with $rc, expected 2"
echo "$out" | grep -q "Could not find your Super Mario 64 ROM" || bad "sm64coin-node did not report the missing ROM"

echo "--- command-line programs"
for p in sm64coind sm64coin-wallet-cli sm64coin-wallet-rpc; do
  ./$p$X --version || bad "$p --version failed"
done

echo "--- wallet app libraries"
case "$OS" in
  linux)
    ldd ./sm64coin-wallet-gui | grep -i 'not found' && bad "wallet app: missing libraries"
    ldd ./sm64coind | grep -i 'not found' && bad "sm64coind: missing libraries"
    gui=./sm64coin-wallet-gui
    # Raspberry Pi OS Bookworm / Debian 12 has glibc 2.36 and libstdc++ 3.4.30: nothing newer may be needed.
    need="$( { for f in sm64coind sm64coin-wallet-cli sm64coin-wallet-rpc sm64coin-wallet-gui sm64coin-leaderboard; do objdump -T "$f"; done; } 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1)"
    needxx="$( { for f in sm64coind sm64coin-wallet-cli sm64coin-wallet-rpc sm64coin-wallet-gui sm64coin-leaderboard; do objdump -T "$f"; done; } 2>/dev/null | grep -o 'GLIBCXX_[0-9.]*' | sort -Vu | tail -1)"
    echo "highest GLIBC needed: $need, highest GLIBCXX needed: $needxx"
    [ "$(printf '%s\n' "$need" GLIBC_2.36 | sort -V | tail -1)" = GLIBC_2.36 ] || bad "needs $need, Raspberry Pi OS Bookworm has glibc 2.36"
    [ -z "$needxx" ] || [ "$(printf '%s\n' "$needxx" GLIBCXX_3.4.30 | sort -V | tail -1)" = GLIBCXX_3.4.30 ] || bad "needs $needxx, Bookworm has libstdc++ 3.4.30" ;;
  windows)
    [ -f sm64coin-wallet-gui.exe ] || bad "wallet app missing"
    ls Qt5Core.dll Qt5Qml.dll Qt5Quick.dll platforms/qwindows.dll >/dev/null || bad "Qt files missing"
    gui=./sm64coin-wallet-gui.exe ;;
  macos)
    app=SM64Coin-Wallet.app/Contents/MacOS/monero-wallet-gui
    [ -x "$app" ] || bad "wallet app missing"
    otool -L "$app" | head -20
    gui="./$app" ;;
esac
# QML loads (headless). Informational: a display-less runner may lack what Qt wants.
qmlcheck() { # qmlcheck LABEL VAR=VALUE...
  local label="$1"; shift
  echo "--- wallet app --check-qml: $label (informational)"
  ( env "$@" HOME="$H" USERPROFILE="$H" "$gui" --check-qml > "$H/qml.log" 2>&1 & p=$!
    for i in $(seq 45); do sleep 1; kill -0 $p 2>/dev/null || break; done
    if kill -0 $p 2>/dev/null; then echo "still running after 45 s (stopped)"; kill $p 2>/dev/null; fi
    wait $p; echo "check-qml exit code $?" )
  tail -n 12 "$H/qml.log" | cut -c1-200
}
case "$OS" in
  windows) qmlcheck "default platform" A=1; qmlcheck "offscreen + software" QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software ;;
  macos) qmlcheck "default platform" A=1
    codesign -vv SM64Coin-Wallet.app 2>&1 | tail -2 ;;
  *) qmlcheck "offscreen" QT_QPA_PLATFORM=offscreen ;;
esac

if [ -n "${SM64COIN_ROM_FILE:-}" ] && [ -f "${SM64COIN_ROM_FILE}" ]; then
  echo "--- full run for 60 s with the ROM (node script, --solo, non-default ports)"
  cp "$SM64COIN_ROM_FILE" "$H/baserom.us.z64"
  if [ "$OS" = windows ]; then
    HOME="$H" USERPROFILE="$H" DATA_DIR="$H\\data" P2P_PORT=29480 RPC_PORT=29481 ZMQ_PORT=29482 cmd //c "sm64coin-node.bat --solo" > "$H/run.log" 2>&1 &
  else
    HOME="$H" ./sm64coin-node --solo --data-dir "$H/data" --p2p-port 29480 --rpc-port 29481 --zmq-port 29482 > "$H/run.log" 2>&1 &
  fi
  pid=$!
  up=0
  for i in $(seq 60); do
    sleep 1
    curl -s -m 3 -d '{"jsonrpc":"2.0","id":0,"method":"get_info"}' http://127.0.0.1:29481/json_rpc | grep -q '"height"' && { up=1; break; }
    kill -0 $pid 2>/dev/null || break
  done
  tail -n 30 "$H/run.log" | cut -c1-200
  [ "$up" = 1 ] || bad "full run: the node did not answer get_info"
  kill $pid 2>/dev/null; sleep 5
  if [ "$OS" = windows ]; then taskkill //F //IM sm64coind.exe >/dev/null 2>&1; fi
  kill -9 $pid 2>/dev/null
else
  echo "(no ROM available: full run skipped)"
fi
exit $fail
