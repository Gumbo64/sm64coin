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
    gui=./sm64coin-wallet-gui
    # Raspberry Pi OS Bookworm / Debian 12 has glibc 2.36 and libstdc++ 3.4.30: nothing newer may be needed.
    need="$( { for f in sm64coin sm64coind sm64coin-wallet-cli sm64coin-wallet-rpc sm64coin-wallet-gui sm64coin-leaderboard; do objdump -T "$f"; done; } 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1)"
    needxx="$( { for f in sm64coin sm64coind sm64coin-wallet-cli sm64coin-wallet-rpc sm64coin-wallet-gui sm64coin-leaderboard; do objdump -T "$f"; done; } 2>/dev/null | grep -o 'GLIBCXX_[0-9.]*' | sort -Vu | tail -1)"
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
    ls SM64Coin-Wallet.app/Contents/Frameworks | head -30
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
  macos)
    qmlcheck "default platform" QT_DEBUG_PLUGINS=1
    grep -i -E "cocoa|dlopen|not loaded|reason|image not found|signature" "$H/qml.log" | cut -c1-300 | head -12
    otool -L SM64Coin-Wallet.app/Contents/PlugIns/platforms/libqcocoa.dylib | cut -c1-160 | head -20
    ls SM64Coin-Wallet.app/Contents/Frameworks | tr '\n' ' '; echo
    codesign -vv SM64Coin-Wallet.app 2>&1 | tail -3 ;;
  *) qmlcheck "offscreen" QT_QPA_PLATFORM=offscreen ;;
esac

# The launcher's own plumbing against a stand-in node (no ROM needed): website, /json_rpc proxy,
# the wallet app as a child process, and that stopping the launcher leaves nothing behind.
echo "--- launcher with a stand-in node"
PY=python3; command -v python3 >/dev/null 2>&1 || PY=python
cat > "$H/mocknode.py" <<'PYEOF'
import http.server, json
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length') or 0))
        b = json.dumps({"jsonrpc": "2.0", "id": 0, "result": {"height": 7, "target_height": 0, "synchronized": True}}).encode()
        self.send_response(200); self.send_header('Content-Type', 'application/json'); self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)
    do_GET = do_POST
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', 27481), H).serve_forever()
PYEOF
"$PY" "$H/mocknode.py" & mockpid=$!
sleep 2
wallet_arg=""; [ "$OS" = linux ] && wallet_arg="--no-wallet"   # no display on the Linux runner
SM64COIN_TEST_NO_ROM=1 HOME="$H" USERPROFILE="$H" ./sm64coin$X --no-browser $wallet_arg --rpc-port 27481 --web-port 27181 --data-dir "$H/plumbing" < /dev/null > "$H/plumbing.log" 2>&1 &
lpid=$!
sleep 12
cat "$H/plumbing.log"
curl -s -m 5 http://127.0.0.1:27181/ | grep -qi "<html\|<!doctype" || bad "plumbing: website not served"
curl -s -m 5 -d '{"jsonrpc":"2.0","id":0,"method":"get_info"}' http://127.0.0.1:27181/json_rpc | grep -q '"height"' || bad "plumbing: /json_rpc proxy"
curl -s -m 5 http://127.0.0.1:27181/some/route | grep -qi "<html\|<!doctype" || bad "plumbing: single-page routes"
if [ "$OS" != linux ]; then
  grep -q "Wallet app started" "$H/plumbing.log" || bad "plumbing: wallet app not started"
  if [ "$OS" = windows ]; then tasklist | grep -i "sm64coin-wallet-gui" > /dev/null && echo "wallet app is running"; else pgrep -f monero-wallet-gui > /dev/null && echo "wallet app is running"; fi
fi
if [ "$OS" = windows ]; then taskkill //F //IM sm64coin.exe; else kill $lpid 2>/dev/null; fi   # Windows: a hard kill, like closing the window
sleep 6
if [ "$OS" = windows ]; then
  tasklist | grep -i "sm64coin"
  tasklist | grep -i "sm64coin-wallet-gui" > /dev/null && bad "plumbing: wallet app still running after the launcher ended"
  taskkill //F //IM sm64coin-wallet-gui.exe >/dev/null 2>&1
elif [ "$OS" = macos ]; then
  pgrep -f monero-wallet-gui > /dev/null && echo "(note: wallet app outlived the launcher killed with SIGTERM)"
fi
kill $mockpid 2>/dev/null

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
